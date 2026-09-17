"""The agent: LangChain's prebuilt agent graph, the gateway's MCP tools, a task.

The graph is `langchain.agents.create_agent`, the LangGraph agent that replaces
`langgraph.prebuilt.create_react_agent` in the 1.x releases. The model writes
the programs; `ExecuteGuard`, a middleware inside that graph, decides what
happens to them. A task may take several `execute` calls (spec section 3.1),
all sharing one `session` id that the agent sets. Every `execute` call is
classified from the raw MCP result, not from the model's reading of it:

- a successful run goes back to the model, which may run another program,
  call `check`, or reply without a tool call, which ends the task;
- a lint error (`isError`, no structured content, text `line N: message`)
  means nothing ran, so it goes back to the model too, until `MAX_LINT_STREAK`
  lint errors in a row;
- a failed run (`isError` with a structured `ExecuteResponse`) means the
  program started and its tool calls may already have happened, so the graph
  ends there and never runs another program, because a retry could transfer
  twice. Anything unrecognised is treated the same way.

At most `MAX_RUNS` runs may succeed, so a confused model cannot keep
transacting, and the model is asked at most `MAX_MODEL_CALLS` times.
"""

from __future__ import annotations

import asyncio
import json
import re
import textwrap
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, TextIO

from langchain.agents import create_agent
from langchain.agents.middleware import AgentMiddleware, hook_config
from langchain_core.language_models import BaseChatModel
from langchain_core.messages import AIMessage, BaseMessage, HumanMessage, ToolMessage
from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_mcp_adapters.interceptors import MCPToolCallRequest
from langchain_mcp_adapters.tools import load_mcp_tools
from mcp.shared.exceptions import McpError
from mcp.types import CallToolResult

SERVER = "proveno-gateway"
MAX_LINT_STREAK = 3
MAX_RUNS = 5
MAX_MODEL_CALLS = 12
WIDTH = 100

SYSTEM_PROMPT = """\
You operate a wallet through the tools of an MCP server. To act, write a \
program for the `execute` tool; its description gives the language and the \
tool API available to you. `check` lints a program without running it. You may \
call `execute` more than once. Reply without calling a tool when the task is done."""

LINT_ERROR = re.compile(r"^line \d+: ")


class Outcome(Enum):
    OK = "ok"
    LINT_ERROR = "lint error"
    RUN_FAILED = "run failed"
    UNEXPECTED = "unexpected error"


def classify(result: CallToolResult) -> Outcome:
    """What an `execute` result means for the next step.

    Only a lint error or a success goes back to the model. Anything that is not
    recognisably one of those is treated as a run that may have had effects."""
    structured = result.structuredContent or {}
    if not result.isError:
        return Outcome.OK if "trace_id" in structured else Outcome.UNEXPECTED
    if "trace_id" in structured:
        return Outcome.RUN_FAILED
    if LINT_ERROR.match(result_text(result)):
        return Outcome.LINT_ERROR
    return Outcome.UNEXPECTED


def result_text(result: CallToolResult) -> str:
    return "".join(getattr(block, "text", "") for block in result.content)


@dataclass
class RawResults:
    """A tool interceptor keeping the raw MCP result of every call, which the
    LangChain tool message does not carry for an error result."""

    results: dict[str, CallToolResult] = field(default_factory=dict)

    async def __call__(
        self,
        request: MCPToolCallRequest,
        handler: Callable[[MCPToolCallRequest], Awaitable[Any]],
    ) -> Any:
        result = await handler(request)
        if isinstance(result, CallToolResult):
            self.results[request.name] = result
        return result


@dataclass
class Run:
    """One `execute` call that reached the gateway."""

    attempt: int
    outcome: Outcome
    trace_id: str | None = None
    result: Any = None
    error: str | None = None


class Stop(Enum):
    ENDED = "the model replied without calling a tool"
    RUN_FAILED = "a run failed, and a failed run is never retried"
    UNEXPECTED = "the gateway returned an error that is not a lint error; not retried"
    LINT_ERRORS = "of too many lint errors in a row"
    RUN_CAP = "the cap on successful runs was reached"
    MODEL_CALLS = "the cap on model calls was reached"


@dataclass
class Report:
    """What happened, for the caller and the tests."""

    session: str
    runs: list[Run] = field(default_factory=list)
    stop: Stop | None = None
    protocol_version: str | None = None
    # tool_call_id -> what the agent made of that `execute` call
    verdicts: dict[str, str] = field(default_factory=dict)

    @property
    def succeeded(self) -> bool:
        return self.stop is Stop.ENDED and any(r.outcome is Outcome.OK for r in self.runs)

    @property
    def trace_ids(self) -> list[str]:
        return [r.trace_id for r in self.runs if r.trace_id]

    def to_json(self) -> dict:
        return {
            "session": self.session,
            "stop": self.stop.name.lower() if self.stop else None,
            "succeeded": self.succeeded,
            "trace_ids": self.trace_ids,
            "runs": [
                {
                    "attempt": r.attempt,
                    "outcome": r.outcome.name.lower(),
                    "trace_id": r.trace_id,
                    "result": r.result,
                    "error": r.error,
                }
                for r in self.runs
            ],
        }


class ExecuteGuard(AgentMiddleware):
    """Enforces the stop rules inside the graph.

    `awrap_tool_call` runs `execute` calls one at a time, sets `request` and
    `session`, classifies the raw result, and refuses to run anything once the
    agent has stopped or the successful-run cap is reached. `abefore_model`
    ends the graph before the model is asked again once the agent has stopped,
    so a model that wants another go after a failed run never gets it."""

    def __init__(
        self,
        task: str,
        raw: RawResults,
        report: Report,
        max_lint_streak: int,
        max_runs: int,
        max_model_calls: int,
    ):
        super().__init__()
        self.task = task
        self.raw = raw
        self.report = report
        self.max_lint_streak = max_lint_streak
        self.max_runs = max_runs
        self.max_model_calls = max_model_calls
        self.lint_streak = 0
        self.successes = 0
        self.model_calls = 0
        # The tool node runs one reply's tool calls concurrently.
        self.lock = asyncio.Lock()

    def _stop(self, why: Stop) -> None:
        if self.report.stop is None:
            self.report.stop = why

    @hook_config(can_jump_to=["end"])
    async def abefore_model(self, state: Any, runtime: Any) -> dict[str, Any] | None:
        if self.report.stop is None and self.model_calls >= self.max_model_calls:
            self._stop(Stop.MODEL_CALLS)
        if self.report.stop is not None:
            return {"jump_to": "end"}
        self.model_calls += 1
        return None

    async def awrap_tool_call(self, request: Any, handler: Callable) -> Any:
        call = request.tool_call
        if call["name"] != "execute":
            return await handler(request)
        async with self.lock:
            report = self.report
            if report.stop is None and self.successes >= self.max_runs:
                self._stop(Stop.RUN_CAP)
            if report.stop is not None:
                reason = f"not run: the agent has stopped, because {report.stop.value}"
                report.verdicts[call["id"]] = "refused; nothing was sent to the gateway"
                return ToolMessage(
                    reason, tool_call_id=call["id"], name=call["name"], status="error"
                )

            attempt = len(report.runs) + 1
            # The task and the session are the agent's, not the model's: every
            # run of this task is recorded under the same session.
            args = {**call["args"], "request": self.task, "session": report.session}
            try:
                message = await handler(request.override(tool_call={**call, "args": args}))
            except McpError as e:
                # A protocol error, such as arguments the gateway rejects, is
                # not a lint error: stop with an outcome rather than a crash.
                report.runs.append(Run(attempt, Outcome.UNEXPECTED, error=str(e)))
                self._stop(Stop.UNEXPECTED)
                report.verdicts[call["id"]] = "not a lint error; the agent stops, no retry"
                return ToolMessage(
                    str(e), tool_call_id=call["id"], name=call["name"], status="error"
                )
            result = self.raw.results.pop("execute")
            outcome = classify(result)
            structured = result.structuredContent or {}
            run = Run(attempt, outcome, trace_id=structured.get("trace_id"))
            report.runs.append(run)
            if outcome is Outcome.OK:
                run.result = structured.get("result")
                self.successes += 1
                self.lint_streak = 0
                verdict = (
                    f"run succeeded ({self.successes} of at most {self.max_runs}); "
                    "the result goes back to the model"
                )
            elif outcome is Outcome.LINT_ERROR:
                run.error = result_text(result)
                self.lint_streak += 1
                if self.lint_streak >= self.max_lint_streak:
                    self._stop(Stop.LINT_ERRORS)
                    verdict = (
                        f"lint error, nothing ran; {self.lint_streak} in a row, so the agent stops"
                    )
                else:
                    verdict = (
                        "lint error, nothing ran; back to the model "
                        f"({self.lint_streak} of {self.max_lint_streak} in a row)"
                    )
            else:
                # A failed run, or an error that cannot be told apart from one:
                # its tool calls may have happened, so nothing runs again.
                run.error = result_text(result)
                failed = outcome is Outcome.RUN_FAILED
                self._stop(Stop.RUN_FAILED if failed else Stop.UNEXPECTED)
                what = "run failed" if failed else "not a lint error"
                verdict = f"{what}, its tool calls may have happened; the agent stops, no retry"
            report.verdicts[call["id"]] = verdict
            return message


def connection(url: str, token: str) -> dict:
    return {
        "transport": "streamable_http",
        "url": url,
        "headers": {"Authorization": f"Bearer {token}"},
    }


async def run(
    model: BaseChatModel,
    task: str,
    url: str,
    token: str,
    out: TextIO,
    max_lint_streak: int = MAX_LINT_STREAK,
    max_runs: int = MAX_RUNS,
    max_model_calls: int = MAX_MODEL_CALLS,
    quiet: bool = False,
) -> Report:
    report = Report(session=f"demo-agent-{uuid.uuid4()}")
    raw = RawResults()
    flow = Flow(out, report, quiet)
    guard = ExecuteGuard(task, raw, report, max_lint_streak, max_runs, max_model_calls)
    client = MultiServerMCPClient({SERVER: connection(url, token)})
    async with client.session(SERVER, auto_initialize=False) as session:
        initialized = await session.initialize()
        report.protocol_version = str(initialized.protocolVersion)
        tools = await load_mcp_tools(session, tool_interceptors=[raw], server_name=SERVER)
        flow.note(
            f"connected to {url}, MCP protocol {report.protocol_version}, "
            f"tools: {', '.join(sorted(t.name for t in tools))}"
        )
        flow.note(
            f"session {report.session}: the agent sets `session` and `request` on every "
            "`execute`, whatever the model passes"
        )
        agent = create_agent(model, tools, system_prompt=SYSTEM_PROMPT, middleware=[guard])
        flow.system(SYSTEM_PROMPT)
        human = HumanMessage(task)
        flow.message(human)
        async for update in agent.astream({"messages": [human]}, stream_mode="updates"):
            for node in update.values():
                for message in (node or {}).get("messages", []):
                    flow.message(message)

    if report.stop is None:
        report.stop = Stop.ENDED
    flow.outcome()
    return report


class Flow:
    """Prints the conversation as the graph streams it, labelled by role."""

    def __init__(self, out: TextIO, report: Report, quiet: bool):
        self.out = out
        self.report = report
        self.quiet = quiet

    def _print(self, text: str = "") -> None:
        if not self.quiet:
            print(text, file=self.out, flush=True)

    def _label(self, role: str) -> None:
        self._print()
        self._print(f"{role} " + "-" * (WIDTH - len(role) - 1))

    def _wrapped(self, text: str, indent: str = "  ") -> None:
        for line in text.splitlines() or [""]:
            wrapped = textwrap.wrap(
                line,
                WIDTH,
                initial_indent=indent,
                subsequent_indent=indent + "  ",
                break_long_words=False,
                break_on_hyphens=False,
            )
            for piece in wrapped or [indent.rstrip()]:
                self._print(piece)

    def note(self, text: str) -> None:
        self._wrapped(f"[{text}]", indent="")

    def system(self, prompt: str) -> None:
        self._label("SYSTEM")
        lines = textwrap.wrap(prompt, WIDTH - 2)
        for line in lines[:4]:
            self._print(f"  {line}")
        shown = "" if len(lines) <= 4 else f"first 4 of {len(lines)} lines, "
        self._print(f"  ({shown}{len(prompt)} characters)")

    def message(self, message: BaseMessage) -> None:
        if isinstance(message, HumanMessage):
            self._label("HUMAN")
            self._wrapped(message.text)
        elif isinstance(message, AIMessage):
            self._label("AI")
            if message.text:
                self._wrapped(message.text)
            for call in message.tool_calls:
                args = dict(call["args"])
                program = args.pop("program", None)
                self._print(f"  tool call: {call['name']} (id {call['id']})")
                if args:
                    self._wrapped(json.dumps(args), indent="    ")
                if program is not None:
                    self._print("    program:")
                    self._print("    ```lua")
                    for line in str(program).rstrip("\n").splitlines():
                        self._print(f"    {line}")
                    self._print("    ```")
        elif isinstance(message, ToolMessage):
            status = ", error" if message.status == "error" else ""
            self._label(f"TOOL {message.name or ''}{status}".replace(" ,", ","))
            self._wrapped(message.text)
            verdict = self.report.verdicts.get(message.tool_call_id)
            if verdict:
                self._print(f"  => {verdict}")

    def outcome(self) -> None:
        """Every run of the task. Printed with `--quiet` too."""
        report = self.report
        self._print()
        self._print("-" * WIDTH)
        lines = [
            f"outcome: {'succeeded' if report.succeeded else 'did not succeed'}; "
            f"stopped because {report.stop.value}",
            f"session: {report.session}",
        ]
        if not report.runs:
            lines.append("  no program reached the gateway")
        for r in report.runs:
            if r.outcome is Outcome.OK:
                detail = f"ok, result {json.dumps(r.result, sort_keys=True)}"
            else:
                first = (r.error or "").strip().splitlines()[:1]
                detail = f"{r.outcome.value}: {first[0] if first else ''}"
            line = f"  attempt {r.attempt}: {detail}"
            if len(line) > WIDTH:
                line = line[: WIDTH - 3] + "..."
            lines.append(line)
            if r.trace_id:
                lines.append(f"    trace_id: {r.trace_id}")
        for line in lines:
            print(line, file=self.out)
        self.out.flush()
