"""The agent loop: a LangChain chat model, the gateway's MCP tools, and a task.

The model writes the program; this code decides what happens to it. Every
`execute` call is classified from the raw MCP result, not from the model's
reading of it:

- a lint error (`isError`, no structured content, text `line N: message`)
  means nothing ran, so the error goes back to the model for another attempt;
- a failed run (`isError` with a structured `ExecuteResponse`) means the
  program started and its tool calls may already have happened, so it is
  reported and never retried, because a retry could transfer twice;
- a successful run ends the loop.
"""

from __future__ import annotations

import json
import re
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, TextIO

from langchain_core.language_models import BaseChatModel
from langchain_core.messages import (
    AIMessage,
    BaseMessage,
    HumanMessage,
    SystemMessage,
    ToolMessage,
)
from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_mcp_adapters.interceptors import MCPToolCallRequest
from langchain_mcp_adapters.tools import load_mcp_tools
from mcp.types import CallToolResult

SERVER = "proveno-gateway"
MAX_ATTEMPTS = 3
# `execute` attempts are capped separately; this bounds `check` calls and
# replies that call no tool at all.
MAX_MODEL_TURNS = 10

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
) -> Report:
    report = Report()
    raw = RawResults()
    client = MultiServerMCPClient({SERVER: connection(url, token)})
    async with client.session(SERVER, auto_initialize=False) as session:
        initialized = await session.initialize()
        report.protocol_version = str(initialized.protocolVersion)
        tools = await load_mcp_tools(session, tool_interceptors=[raw], server_name=SERVER)
        by_name = {tool.name: tool for tool in tools}
        section(out, f"Connected to {url}, MCP protocol {report.protocol_version}")
        print("Tools: " + ", ".join(sorted(by_name)), file=out)

        bound = model.bind_tools(tools)
        messages: list[BaseMessage] = [SystemMessage(SYSTEM_PROMPT), HumanMessage(task)]
        for _ in range(MAX_MODEL_TURNS):
            reply = await bound.ainvoke(messages)
            messages.append(reply)
            if not isinstance(reply, AIMessage) or not reply.tool_calls:
                report.error = "the model replied without calling `execute`"
                section(out, "The model replied without running a program")
                print(reply.text, file=out)
                return report
            for call in reply.tool_calls:
                tool = by_name.get(call["name"])
                if tool is None:
                    messages.append(
                        ToolMessage(
                            f"unknown tool `{call['name']}`",
                            tool_call_id=call["id"],
                            status="error",
                        )
                    )
                    continue
                if call["name"] != "execute":
                    section(out, f"The model called `{call['name']}`")
                    messages.append(await tool.ainvoke(call))
                    print(result_text(raw.results[call["name"]]), file=out)
                    continue

                report.attempts += 1
                args = dict(call["args"])
                # The task is recorded in the trace by the agent, not left to
                # the model to pass along.
                args["request"] = task
                program = str(args.get("program", ""))
                section(out, f"Attempt {report.attempts}: the program the model wrote")
                print(program.rstrip("\n"), file=out)

                messages.append(await tool.ainvoke({**call, "args": args}))
                result = raw.results["execute"]
                outcome = classify(result)
                report.outcome = outcome
                if outcome is Outcome.OK:
                    report.response = result.structuredContent
                    section(out, "The tool result")
                    print(json.dumps(report.response, indent=2), file=out)
                    print(f"\ntrace_id: {report.response['trace_id']}", file=out)
                    return report
                if outcome is Outcome.LINT_ERROR:
                    report.error = result_text(result)
                    section(out, "Lint error: nothing ran, so the model may fix it")
                    print(report.error, file=out)
                    if report.attempts >= max_attempts:
                        print(f"\nNo program linted in {max_attempts} attempts.", file=out)
                        return report
                    continue
                # A failed run, or an error that cannot be told apart from
                # one: its tool calls may have happened, so stop here.
                report.error = result_text(result)
                report.response = result.structuredContent
                if outcome is Outcome.RUN_FAILED:
                    title = "The run failed after it started"
                else:
                    title = "The gateway returned an error that is not a lint error"
                section(out, f"{title}: its tool calls may have happened, so it is not retried")
                print(report.error, file=out)
                if report.response is not None:
                    print(f"\ntrace_id: {report.response['trace_id']}", file=out)
                return report
        report.error = f"no run after {MAX_MODEL_TURNS} model turns"
        section(out, report.error)
        return report


def section(out: TextIO, title: str) -> None:
    print(f"\n--- {title}\n", file=out, flush=True)
