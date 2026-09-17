"""The agent: LangChain's prebuilt agent graph, the gateway's MCP tools, a task.

The graph is `langchain.agents.create_agent`, the LangGraph agent that replaces
`langgraph.prebuilt.create_react_agent` in the 1.x releases. The model writes
the program; `ExecuteGuard`, a middleware inside that graph, decides what
happens to it. Every `execute` call is classified from the raw MCP result, not
from the model's reading of it:

- a lint error (`isError`, no structured content, text `line N: message`)
  means nothing ran, so the error goes back to the model for another attempt,
  up to `MAX_ATTEMPTS`;
- a failed run (`isError` with a structured `ExecuteResponse`) means the
  program started and its tool calls may already have happened, so the graph
  ends there and never runs another program, because a retry could transfer
  twice. Anything unrecognised is treated the same way;
- a successful run also ends the graph.
"""

from __future__ import annotations

import asyncio
import json
import re
import textwrap
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, TextIO

from langchain.agents import create_agent
from langchain.agents.middleware import AgentMiddleware, ModelCallLimitMiddleware, hook_config
from langchain_core.language_models import BaseChatModel
from langchain_core.messages import AIMessage, BaseMessage, HumanMessage, ToolMessage
from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_mcp_adapters.interceptors import MCPToolCallRequest
from langchain_mcp_adapters.tools import load_mcp_tools
from mcp.types import CallToolResult

SERVER = "proveno-gateway"
MAX_ATTEMPTS = 3
# `execute` attempts are capped by `ExecuteGuard`; this bounds `check` calls.
MAX_MODEL_CALLS = 10
WIDTH = 100

SYSTEM_PROMPT = """\
You operate a wallet through the tools of an MCP server. To act, write a \
program for the `execute` tool; its description gives the language and the \
tool API available to you. `check` lints a program without running it."""

LINT_ERROR = re.compile(r"^line \d+: ")


class Outcome(Enum):
    OK = "ok"
    LINT_ERROR = "lint error"
    RUN_FAILED = "run failed"
    UNEXPECTED = "unexpected error"


def classify(result: CallToolResult) -> Outcome:
    """What an `execute` result means for the next step.

    Only a lint error is safe to retry. Anything that is not recognisably a
    lint error is treated as a run that may have had effects."""
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
class Report:
    """What happened, for the caller and the tests."""

    outcome: Outcome | None = None
    attempts: int = 0
    protocol_version: str | None = None
    response: dict | None = None
    error: str | None = None
    # tool_call_id -> the classification of that `execute` call
    verdicts: dict[str, str] = field(default_factory=dict)


class ExecuteGuard(AgentMiddleware):
    """Enforces the retry rule inside the graph.

    `awrap_tool_call` runs each `execute` call one at a time, sets `request` to
    the task, classifies the raw result and refuses to run anything once the
    agent has stopped. `abefore_model` ends the graph before the model is asked
    again, so a model that wants another go never gets the chance."""

    def __init__(self, task: str, raw: RawResults, report: Report, max_attempts: int):
        super().__init__()
        self.task = task
        self.raw = raw
        self.report = report
        self.max_attempts = max_attempts
        self.stopped = False
        # The tool node runs one reply's tool calls concurrently.
        self.lock = asyncio.Lock()

    @hook_config(can_jump_to=["end"])
    async def abefore_model(self, state: Any, runtime: Any) -> dict[str, Any] | None:
        return {"jump_to": "end"} if self.stopped else None

    async def awrap_tool_call(self, request: Any, handler: Callable) -> Any:
        call = request.tool_call
        if call["name"] != "execute":
            return await handler(request)
        async with self.lock:
            report = self.report
            if self.stopped:
                reason = "not run: the agent has already stopped, and runs no further programs"
                report.verdicts[call["id"]] = reason
                return ToolMessage(reason, tool_call_id=call["id"], status="error")

            report.attempts += 1
            # The task is recorded in the trace by the agent, not left to the
            # model to pass along.
            args = {**call["args"], "request": self.task}
            message = await handler(request.override(tool_call={**call, "args": args}))
            result = self.raw.results.pop("execute")
            outcome = classify(result)
            report.outcome = outcome
            if outcome is Outcome.OK:
                self.stopped = True
                report.response = result.structuredContent
                verdict = "run succeeded; the agent stops"
            elif outcome is Outcome.LINT_ERROR:
                report.error = result_text(result)
                if report.attempts >= self.max_attempts:
                    self.stopped = True
                    verdict = (
                        f"lint error, nothing ran; no program linted in {self.max_attempts} "
                        "attempts, so the agent stops"
                    )
                else:
                    verdict = (
                        f"lint error, nothing ran; back to the model "
                        f"(attempt {report.attempts} of {self.max_attempts})"
                    )
            else:
                # A failed run, or an error that cannot be told apart from one:
                # its tool calls may have happened, so nothing runs again.
                self.stopped = True
                report.error = result_text(result)
                report.response = result.structuredContent
                what = "run failed" if outcome is Outcome.RUN_FAILED else "not a lint error"
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
    max_attempts: int = MAX_ATTEMPTS,
    quiet: bool = False,
) -> Report:
    report = Report()
    raw = RawResults()
    flow = Flow(out, report, quiet)
    client = MultiServerMCPClient({SERVER: connection(url, token)})
    async with client.session(SERVER, auto_initialize=False) as session:
        initialized = await session.initialize()
        report.protocol_version = str(initialized.protocolVersion)
        tools = await load_mcp_tools(session, tool_interceptors=[raw], server_name=SERVER)
        flow.note(
            f"connected to {url}, MCP protocol {report.protocol_version}, "
            f"tools: {', '.join(sorted(t.name for t in tools))}"
        )
        agent = create_agent(
            model,
            tools,
            system_prompt=SYSTEM_PROMPT,
            middleware=[
                ExecuteGuard(task, raw, report, max_attempts),
                ModelCallLimitMiddleware(run_limit=MAX_MODEL_CALLS, exit_behavior="end"),
            ],
        )
        flow.system(SYSTEM_PROMPT)
        human = HumanMessage(task)
        flow.message(human)
        async for update in agent.astream({"messages": [human]}, stream_mode="updates"):
            for node in update.values():
                for message in (node or {}).get("messages", []):
                    flow.message(message)

    if report.outcome is None:
        report.error = "the model never called `execute`"
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
        for line in lines[:3]:
            self._print(f"  {line}")
        shown = "" if len(lines) <= 3 else f"first 3 of {len(lines)} lines, "
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
        report = self.report
        self._print()
        self._print("-" * WIDTH)
        if report.outcome is Outcome.OK:
            trace = report.response["trace_id"]
            print(f"outcome: run succeeded after {report.attempts} attempt(s)", file=self.out)
            print(f"trace_id: {trace}", file=self.out, flush=True)
            return
        label = report.outcome.value if report.outcome else "no run"
        print(f"outcome: {label} after {report.attempts} attempt(s)", file=self.out)
        if report.response and "trace_id" in report.response:
            print(f"trace_id: {report.response['trace_id']}", file=self.out)
        self.out.flush()
