"""Integration tests: the agent loop against a real gateway, driven by a
scripted LangChain chat model instead of a paid one."""

from __future__ import annotations

import asyncio
import io
from pathlib import Path

from conftest import conformance
from langchain_core.messages import AIMessage, ToolMessage
from scripted import ScriptedChatModel, call, execute

from demo_agent.agent import Outcome, run

REBALANCE = (Path(__file__).resolve().parents[2] / "rebalance.lua").read_text()
# `os` is not in the dialect, so this is rejected before anything runs.
NOT_IN_THE_DIALECT = """\
local started = os.time()
return { started = started }
"""
# Transfers, then fails: the transfer has happened by the time the run fails.
TRANSFERS_THEN_FAILS = """\
wallet.transfer{ to = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8", amount = 5 }
error("something went wrong after the transfer")
"""


def agent_run(world, model) -> tuple:
    world.set_balances(world.fixture["balances_milli"])
    vault_before = world.balance_wei(conformance.VAULT)
    out = io.StringIO()
    token = world.env["DEMO_AGENT_TOKEN"]
    report = asyncio.run(run(model, world.fixture["task"], world.gateway_url, token, out))
    vault_gain = (world.balance_wei(conformance.VAULT) - vault_before) // conformance.MILLI_WEI
    return report, out.getvalue(), vault_gain


def test_lint_error_goes_back_to_the_model_and_the_corrected_program_runs(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT), execute(REBALANCE)], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.outcome is Outcome.OK, out
    assert report.attempts == 2
    # The model saw the lint error as the result of its first call.
    feedback = model.received[1][-1]
    assert isinstance(feedback, ToolMessage) and feedback.status == "error"
    assert feedback.text.startswith("line 1: "), feedback.text
    trace_id = report.response["trace_id"]
    assert report.response["result"]["action"] == "rebalanced"
    assert vault_gain == 20
    assert (world.out / "traces" / "traces" / f"{trace_id}.json").exists()


def test_negotiates_the_protocol_the_gateway_caps_at(world):
    model = ScriptedChatModel(responses=[execute(REBALANCE)], received=[])
    report, _, _ = agent_run(world, model)
    assert report.protocol_version == "2025-11-25"


def test_failed_run_is_not_retried(world):
    # The script would run the program a second time if the loop let it.
    model = ScriptedChatModel(
        responses=[execute(TRANSFERS_THEN_FAILS), execute(TRANSFERS_THEN_FAILS)], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.outcome is Outcome.RUN_FAILED, out
    assert len(model.received) == 1
    assert report.attempts == 1
    assert vault_gain == 5
    assert "already happened" in report.error
    assert "the agent stops, no retry" in out
    assert report.response["trace_id"] in out


def test_gives_up_after_three_lint_errors(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT) for _ in range(4)], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.outcome is Outcome.LINT_ERROR
    assert report.attempts == 3
    assert len(model.received) == 3
    assert vault_gain == 0
    assert "no program linted in 3 attempts" in out


def test_check_calls_are_answered_and_do_not_count_as_attempts(world):
    model = ScriptedChatModel(
        responses=[call("check", NOT_IN_THE_DIALECT), execute(REBALANCE)], received=[]
    )
    report, out, vault_gain = agent_run(world, model)

    assert report.outcome is Outcome.OK, out
    assert report.attempts == 1
    feedback = model.received[1][-1]
    assert isinstance(feedback, ToolMessage) and feedback.text.startswith("line 1: ")
    assert vault_gain == 20


def test_a_second_execute_in_the_same_reply_does_not_run_after_a_failed_run(world):
    # Both calls arrive in one reply, so the tool node would run them together.
    first, second = execute(TRANSFERS_THEN_FAILS), execute(TRANSFERS_THEN_FAILS)
    both = AIMessage(content="", tool_calls=first.tool_calls + second.tool_calls)
    model = ScriptedChatModel(responses=[both, execute(REBALANCE)], received=[])
    report, out, vault_gain = agent_run(world, model)

    assert report.outcome is Outcome.RUN_FAILED, out
    assert report.attempts == 1
    assert len(model.received) == 1
    assert vault_gain == 5
    assert "TOOL, error" not in out
    assert "not run: the agent has already stopped" in out


def test_an_execute_call_the_gateway_rejects_stops_the_agent(world):
    rejected = AIMessage(
        content="", tool_calls=[{"name": "execute", "args": {"prog": "x"}, "id": "call_bad"}]
    )
    model = ScriptedChatModel(responses=[rejected, execute(REBALANCE)], received=[])
    report, out, vault_gain = agent_run(world, model)

    assert report.outcome is Outcome.UNEXPECTED, out
    assert len(model.received) == 1
    assert vault_gain == 0
    assert "invalid arguments" in report.error
    assert "=> not a lint error; the agent stops, no retry" in out


def test_prints_the_message_flow_in_order_as_the_model_receives_it(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT), execute(REBALANCE)], received=[]
    )
    report, out, _ = agent_run(world, model)
    assert report.outcome is Outcome.OK, out

    # What the model was sent on its second call: the lint error, as a tool message.
    lint = model.received[1][-1]
    assert isinstance(lint, ToolMessage) and lint.text.startswith("line 1: ")
    trace_id = report.response["trace_id"]
    markers = [
        "SYSTEM ",
        "HUMAN ",
        world.fixture["task"][:40],
        "AI ",
        "tool call: execute",
        "```lua",
        "local started = os.time()",
        "TOOL execute, error ",
        lint.text,
        "=> lint error, nothing ran; back to the model (attempt 1 of 3)",
        "AI ",
        "tool call: execute",
        'local quote = market.get_price{ pair = "ETH/USD" }',
        "TOOL execute ",
        f'"trace_id":"{trace_id}"',
        "=> run succeeded; the agent stops",
        f"trace_id: {trace_id}",
    ]
    position = 0
    for marker in markers:
        found = out.find(marker, position)
        assert found != -1, f"{marker!r} missing after offset {position}:\n{out}"
        position = found + len(marker)


def test_quiet_prints_only_the_outcome(world):
    model = ScriptedChatModel(
        responses=[execute(NOT_IN_THE_DIALECT), execute(REBALANCE)], received=[]
    )
    world.set_balances(world.fixture["balances_milli"])
    out = io.StringIO()
    token = world.env["DEMO_AGENT_TOKEN"]
    report = asyncio.run(
        run(model, world.fixture["task"], world.gateway_url, token, out, quiet=True)
    )
    trace_id = report.response["trace_id"]
    assert out.getvalue() == (f"outcome: run succeeded after 2 attempt(s)\ntrace_id: {trace_id}\n")
