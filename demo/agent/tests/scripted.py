"""A scripted LangChain chat model: LangChain's own fake model, able to take
tools, that replies from a fixed list and records what it was sent."""

from __future__ import annotations

import itertools
from typing import Any

from langchain_core.language_models.fake_chat_models import FakeMessagesListChatModel
from langchain_core.messages import AIMessage, BaseMessage
from langchain_core.outputs import ChatResult

_ids = itertools.count(1)


def call(tool: str, program: str) -> AIMessage:
    """A reply calling `tool` with this program."""
    return AIMessage(
        content="",
        tool_calls=[{"name": tool, "args": {"program": program}, "id": f"call_{next(_ids)}"}],
    )


def execute(program: str) -> AIMessage:
    return call("execute", program)


class ScriptedChatModel(FakeMessagesListChatModel):
    received: list[list[BaseMessage]] = []

    def bind_tools(self, tools: Any, **kwargs: Any) -> ScriptedChatModel:
        return self

    def _generate(self, messages: list[BaseMessage], *args: Any, **kwargs: Any) -> ChatResult:
        if len(self.received) >= len(self.responses):
            raise AssertionError(f"the script has only {len(self.responses)} replies")
        self.received.append(list(messages))
        return super()._generate(messages, *args, **kwargs)
