"""Unit tests: telling a lint error from a failed run, and model configuration."""

import pytest
from mcp.types import CallToolResult, TextContent

from demo_agent.agent import Outcome, classify
from demo_agent.cli import ConfigError, build_model


def text(s: str) -> list[TextContent]:
    return [TextContent(type="text", text=s)]


def test_successful_run_is_ok():
    result = CallToolResult(
        content=text("{}"),
        structuredContent={"result": {}, "trace_id": "t1", "status": {"type": "ok"}},
    )
    assert classify(result) is Outcome.OK


def test_lint_error_is_retryable():
    # The text of `ExecuteError::Lint` in src/server.rs: `line N: message`, no
    # structured content.
    result = CallToolResult(content=text("line 4: unknown global `os`"), isError=True)
    assert classify(result) is Outcome.LINT_ERROR


def test_failed_run_is_not_a_lint_error_even_when_it_names_a_line():
    # A failed run's text in src/server.rs leads with the kind, and its message
    # can itself start `line N:`; the structured response marks it as a run.
    status = {"type": "error", "kind": "RuntimeError", "message": "line 3: boom"}
    result = CallToolResult(
        content=text(
            "RuntimeError: line 3: boom\nThe run is recorded as trace t2, and any tool calls "
            "it made before failing have already happened, so check them before running it "
            "again."
        ),
        structuredContent={"result": None, "trace_id": "t2", "status": status},
        isError=True,
    )
    assert classify(result) is Outcome.RUN_FAILED


def test_unrecognised_error_is_not_retryable():
    result = CallToolResult(content=text("line 3 went wrong"), isError=True)
    assert classify(result) is Outcome.UNEXPECTED
    result = CallToolResult(content=text("RuntimeError: line 3: boom"), isError=True)
    assert classify(result) is Outcome.UNEXPECTED


def test_missing_key_names_the_variable(monkeypatch):
    monkeypatch.delenv("SOME_KEY", raising=False)
    with pytest.raises(ConfigError, match="SOME_KEY is not set"):
        build_model("openai", "qwen/qwen3-coder", None, "SOME_KEY", 1000)


def test_openai_compatible_endpoint_is_configuration(monkeypatch):
    monkeypatch.setenv("SOME_KEY", "sk-test-not-a-real-key")
    model = build_model("openai", "qwen/qwen3-coder", "http://localhost:11434/v1", "SOME_KEY", 100)
    assert model.model_name == "qwen/qwen3-coder"
    assert model.openai_api_base == "http://localhost:11434/v1"
    assert "sk-test-not-a-real-key" not in repr(model)


def test_openai_defaults_to_openrouter(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-test-not-a-real-key")
    model = build_model("openai", "qwen/qwen3-coder", None, None, 100)
    assert model.openai_api_base == "https://openrouter.ai/api/v1"


def test_anthropic_is_the_default_provider(monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-test-not-a-real-key")
    model = build_model("anthropic", None, None, None, 100)
    assert model.model == "claude-opus-5"
    assert "sk-ant-test-not-a-real-key" not in repr(model)
