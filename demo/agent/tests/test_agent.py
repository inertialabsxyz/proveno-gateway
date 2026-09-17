"""Integration tests: the agent graph against a real gateway, driven by a
scripted LangChain chat model instead of a paid one."""

from __future__ import annotations

import asyncio
import io
import json
from pathlib import Path

from conftest import TASK, conformance, run_sh
from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from scripted import ScriptedChatModel, call, execute, reply

from demo_agent.agent import MAX_REPLY_CHARS, Outcome, Stop, run, shortened_reply

REBALANCE = (Path(__file__).resolve().parents[2] / "rebalance.lua").read_text()
# `os` is not in the dialect, so this is rejected before anything runs.
NOT_IN_THE_DIALECT = """\
local started = os.time()
return { started = started }
"""
# Reads only: the first half of a task split across two runs.
READ_ONLY = """\
local quote = market.get_price{ pair = "ETH/USD" }
local hot = wallet.get_balance{ address = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266" }
return { price = quote.price, hot = hot.eth_milli }
"""
TRANSFER_20 = """\
local sent = wallet.transfer{ to = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8", amount = 20 }
return { tx_hash = sent.tx_hash }
"""
TRANSFER_5 = TRANSFER_20.replace("amount = 20", "amount = 5")
# Transfers, then fails: the transfer has happened by the time the run fails.
TRANSFERS_THEN_FAILS = """\
wallet.transfer{ to = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8", amount = 5 }
error("something went wrong after the transfer")
"""


def agent_run(world, model, **kwargs) -> tuple:
    world.set_balances(world.fixture["balances_milli"])
    vault_before = world.balance_wei(conformance.VAULT)
    out = io.StringIO()
    token = world.env["DEMO_AGENT_TOKEN"]
    report = asyncio.run(run(model, TASK, world.gateway_url, token, out, **kwargs))
    vault_gain = (world.balance_wei(conformance.VAULT) - vault_before) // conformance.MILLI_WEI
    return report, out.getvalue(), vault_gain


def trace(world, trace_id: str) -> dict:
    return json.loads((world.out / "traces" / "traces" / f"{trace_id}.json").read_text())


def outcomes(report) -> list[Outcome]:
    return [r.outcome for r in report.runs]


def test_a_task_split_across_two_runs_shares_one_session(world):
    model = ScriptedChatModel(
        responses=[execute(READ_ONLY), execute(TRANSFER_20), reply("Rebalanced.")], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.ENDED and report.succeeded, out
    assert outcomes(report) == [Outcome.OK, Outcome.OK]
    assert len(model.received) == 3
    # The model saw the first run's result before it wrote the second program.
    first = model.received[1][-1]
    assert isinstance(first, ToolMessage) and '"hot":620' in first.text
    assert vault_gain == 20
    assert len(report.trace_ids) == 2
    headers = [trace(world, t)["header"] for t in report.trace_ids]
    assert [h["session"] for h in headers] == [report.session, report.session]
    assert report.session.startswith("demo-agent-")


def test_the_session_is_the_agents_whatever_the_model_passes(world):
    own = AIMessage(
        content="",
        tool_calls=[
            {
                "name": "execute",
                "args": {"program": READ_ONLY, "session": "model-chosen", "request": "other"},
                "id": "call_own_session",
            }
        ],
    )
    model = ScriptedChatModel(responses=[own, reply("Done.")], received=[])
    report, out, _ = agent_run(world, model)

    header = trace(world, report.trace_ids[0])["header"]
    assert header["session"] == report.session, out


def test_lint_error_goes_back_to_the_model_and_the_corrected_program_runs(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT), execute(REBALANCE), reply("Done.")], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.succeeded, out
    assert outcomes(report) == [Outcome.LINT_ERROR, Outcome.OK]
    feedback = model.received[1][-1]
    assert isinstance(feedback, ToolMessage) and feedback.status == "error"
    assert feedback.text.startswith("line 1: "), feedback.text
    assert report.runs[1].result["action"] == "rebalanced"
    assert vault_gain == 20


def test_negotiates_the_protocol_the_gateway_caps_at(world):
    model = ScriptedChatModel(responses=[reply("Nothing to do.")], received=[])
    report, _, _ = agent_run(world, model)
    assert report.protocol_version == "2025-11-25"


def test_failed_run_is_not_retried(world):
    # The script would run the program a second time if the graph let it.
    model = ScriptedChatModel(
        responses=[execute(TRANSFERS_THEN_FAILS), execute(TRANSFERS_THEN_FAILS)], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.RUN_FAILED and not report.succeeded, out
    assert outcomes(report) == [Outcome.RUN_FAILED]
    assert len(model.received) == 1
    assert vault_gain == 5
    assert "already happened" in report.runs[0].error
    assert "the agent stops, no retry" in out
    assert report.trace_ids[0] in out


def test_a_successful_run_then_a_failed_run_stops_the_agent(world):
    model = ScriptedChatModel(
        responses=[execute(READ_ONLY), execute(TRANSFERS_THEN_FAILS), execute(TRANSFER_20)],
        received=[],
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.RUN_FAILED, out
    assert outcomes(report) == [Outcome.OK, Outcome.RUN_FAILED]
    assert len(model.received) == 2
    assert vault_gain == 5


def test_the_successful_run_cap_bounds_real_transfers(world):
    model = ScriptedChatModel(
        responses=[execute(TRANSFER_5) for _ in range(4)] + [reply("Done.")], received=[]
    )
    report, out, vault_gain = agent_run(world, model, max_runs=2)

    assert report.stop is Stop.RUN_CAP and not report.succeeded, out
    assert outcomes(report) == [Outcome.OK, Outcome.OK]
    # The third call is refused without reaching the gateway, and the model is
    # not asked again.
    assert len(model.received) == 3
    assert vault_gain == 10
    assert "not run: the agent has stopped, because the cap on successful runs" in out


def test_gives_up_after_three_lint_errors_in_a_row(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT) for _ in range(4)], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.LINT_ERRORS
    assert outcomes(report) == [Outcome.LINT_ERROR] * 3
    assert len(model.received) == 3
    assert vault_gain == 0
    assert "3 in a row, so the agent stops" in out


def test_a_successful_run_resets_the_lint_error_streak(world):
    # Two lint errors, a success, two more lint errors: never three in a row.
    programs = [NOT_IN_THE_DIALECT, NOT_IN_THE_DIALECT, READ_ONLY]
    programs += [NOT_IN_THE_DIALECT, NOT_IN_THE_DIALECT, TRANSFER_20]
    model = ScriptedChatModel(
        responses=[execute(p) for p in programs] + [reply("Done.")], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.ENDED and report.succeeded, out
    assert outcomes(report).count(Outcome.LINT_ERROR) == 4
    assert vault_gain == 20
    assert "(2 of 3 in a row)" in out


def test_check_calls_are_answered_and_do_not_count_as_runs(world):
    model = ScriptedChatModel(
        responses=[call("check", NOT_IN_THE_DIALECT), execute(REBALANCE), reply("Done.")],
        received=[],
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.succeeded, out
    assert outcomes(report) == [Outcome.OK]
    feedback = model.received[1][-1]
    assert isinstance(feedback, ToolMessage) and feedback.text.startswith("line 1: ")
    assert vault_gain == 20


def test_a_second_execute_in_the_same_reply_does_not_run_after_a_failed_run(world):
    # Both calls arrive in one reply, so the tool node would run them together.
    first, second = execute(TRANSFERS_THEN_FAILS), execute(TRANSFERS_THEN_FAILS)
    both = AIMessage(content="", tool_calls=first.tool_calls + second.tool_calls)
    model = ScriptedChatModel(responses=[both, execute(REBALANCE)], received=[])
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.RUN_FAILED, out
    assert outcomes(report) == [Outcome.RUN_FAILED]
    assert len(model.received) == 1
    assert vault_gain == 5
    assert "TOOL, error" not in out
    assert "not run: only the first `execute` of a reply runs" in out


def test_a_second_execute_in_the_same_reply_is_refused_after_a_successful_run(world):
    # The model has not seen the first run's result, so the second is not sent.
    first, second = execute(TRANSFER_5), execute(TRANSFER_5)
    both = AIMessage(content="", tool_calls=first.tool_calls + second.tool_calls)
    model = ScriptedChatModel(responses=[both, reply("Done.")], received=[])
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.ENDED and report.succeeded, out
    assert outcomes(report) == [Outcome.OK]
    assert vault_gain == 5
    assert len(model.received) == 2
    refused = model.received[1][-1]
    assert isinstance(refused, ToolMessage) and refused.tool_call_id == second.tool_calls[0]["id"]
    assert refused.text.startswith("not run: only the first `execute` of a reply runs"), out


def test_an_execute_call_the_gateway_rejects_stops_the_agent(world):
    rejected = AIMessage(
        content="", tool_calls=[{"name": "execute", "args": {"prog": "x"}, "id": "call_bad"}]
    )
    model = ScriptedChatModel(responses=[rejected, execute(REBALANCE)], received=[])
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.UNEXPECTED, out
    assert len(model.received) == 1
    assert vault_gain == 0
    assert "invalid arguments" in report.runs[0].error
    assert "=> not a lint error; the agent stops, no retry" in out


def test_prints_the_message_flow_in_order_as_the_model_receives_it(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT), execute(READ_ONLY), execute(TRANSFER_20)]
        + [reply("Rebalanced: 20 milli-ETH to the vault.")],
        received=[],
    )
    report, out, _ = agent_run(world, model)
    assert report.succeeded, out

    # What the model was sent: the lint error, then each run's result.
    lint = model.received[1][-1]
    read = model.received[2][-1]
    sent = model.received[3][-1]
    assert isinstance(lint, ToolMessage) and lint.text.startswith("line 1: ")
    first_trace, second_trace = report.trace_ids
    markers = [
        f"session {report.session}",
        "SYSTEM ",
        "HUMAN ",
        TASK[:40],
        "AI ",
        "tool call: execute",
        "```lua",
        "local started = os.time()",
        "TOOL execute, error ",
        lint.text,
        "=> lint error, nothing ran; back to the model (1 of 3 in a row)",
        "AI ",
        'local quote = market.get_price{ pair = "ETH/USD" }',
        "TOOL execute ",
        read.text,
        "=> run succeeded (1 of at most 5); the result goes back to the model",
        "AI ",
        "wallet.transfer{",
        "TOOL execute ",
        sent.text,
        "=> run succeeded (2 of at most 5)",
        "AI ",
        "Rebalanced: 20 milli-ETH to the vault.",
        "outcome: succeeded; stopped because the model replied without calling a tool",
        f"session: {report.session}",
        "attempt 1: lint error: line 1: ",
        "attempt 2: ok, result ",
        f"trace_id: {first_trace}",
        "attempt 3: ok, result ",
        f"trace_id: {second_trace}",
    ]
    position = 0
    for marker in markers:
        found = out.find(marker, position)
        assert found != -1, f"{marker!r} missing after offset {position}:\n{out}"
        position = found + len(marker)


def test_quiet_prints_only_the_outcome(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT), execute(READ_ONLY), reply("Done.")], received=[]
    )
    report, out, _ = agent_run(world, model, quiet=True)
    [trace_id] = report.trace_ids
    lines = out.splitlines()
    assert lines[:3] == [
        "outcome: succeeded; stopped because the model replied without calling a tool",
        f"session: {report.session}",
        "  attempt 1: lint error: line 1: `os` is not available; time and randomness are tool "
        "calls",
    ]
    assert lines[3].startswith("  attempt 2: ok, result {")
    assert lines[4:] == [f"    trace_id: {trace_id}", "final reply from the model:", "  Done."]


def test_the_model_is_given_exactly_the_task_run_sh_sends(world):
    model = ScriptedChatModel(responses=[reply("Nothing to do.")], received=[])
    agent_run(world, model)

    [human] = [m for m in model.received[0] if isinstance(m, HumanMessage)]
    assert human.text == run_sh("--print-task").rstrip("\n")
    # The task names the two accounts this world funds, the ones run.sh funds.
    assert conformance.HOT in human.text and conformance.VAULT in human.text


def test_a_model_that_reads_then_stops_has_its_reply_in_the_summary(world, tmp_path):
    stopped = "I read the hot wallet's balance, but I cannot see the vault, so I made no transfer."
    model = ScriptedChatModel(responses=[execute(READ_ONLY), reply(stopped)], received=[])
    report, out, vault_gain = agent_run(world, model)

    assert report.stop is Stop.ENDED, out
    assert vault_gain == 0
    assert report.final_reply == stopped
    summary = out[out.index("outcome: ") :]
    assert f"final reply from the model:\n  {stopped}\n" in summary
    assert report.to_json()["final_reply"] == stopped

    # What run.sh reports when no run made a transfer, from the result file.
    result_file = tmp_path / "agent-result.json"
    result_file.write_text(json.dumps(report.to_json()))
    message = run_sh("--no-transfer-message", str(result_file))
    assert message == f"no run made a transfer; the model's final reply was:\n{stopped}\n"


def test_no_final_reply_when_the_agent_stops_itself(world):
    model = ScriptedChatModel(
        responses=[execute(TRANSFERS_THEN_FAILS), reply("unused")], received=[]
    )
    report, out, _ = agent_run(world, model)
    assert report.stop is Stop.RUN_FAILED
    assert report.final_reply is None
    assert "final reply from the model" not in out


def test_a_long_final_reply_is_truncated_and_says_so():
    long = "word " * 1000
    short = shortened_reply(long)
    assert short.startswith(long[:100])
    assert short.endswith(f"[truncated: the first {MAX_REPLY_CHARS} of {len(long)} characters]")
    assert shortened_reply("brief") == "brief"
