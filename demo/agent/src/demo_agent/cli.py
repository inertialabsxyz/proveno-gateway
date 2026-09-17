"""`demo-agent TASK`: a model writes a program for the gateway and runs it.

The model is configuration, not code, following demo/conformance: `--api`
picks the request shape (`anthropic`, or `openai` for any OpenAI-compatible
endpoint such as OpenRouter), `--base-url` the endpoint, `--api-key-var` the
environment variable holding the key, and `--model` the model id. Each flag
also reads a `DEMO_AGENT_*` environment variable, so `demo/run.sh` can pass
the choice through.

The API key is read from the environment and handed to the chat model in
memory. It is never printed, written to a file, or put on a command line.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import sys
from pathlib import Path

from langchain_core.language_models import BaseChatModel
from pydantic import SecretStr

from demo_agent.agent import MAX_LINT_STREAK, MAX_RUNS, run

API_DEFAULTS = {
    "anthropic": (None, "ANTHROPIC_API_KEY"),
    "openai": ("https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
}
DEFAULT_MODELS = {"anthropic": "claude-opus-5"}
TOKEN_VAR = "DEMO_AGENT_TOKEN"
DEFAULT_URL = "http://127.0.0.1:7777/mcp"


class ConfigError(Exception):
    pass


def key_var(api: str, override: str | None) -> str:
    return override or API_DEFAULTS[api][1]


def build_model(
    api: str, model: str | None, base_url: str | None, api_key_var: str | None, max_tokens: int
) -> BaseChatModel:
    var = key_var(api, api_key_var)
    key = os.environ.get(var)
    if not key:
        raise ConfigError(f"{var} is not set; it holds the model API key")
    model = model or DEFAULT_MODELS.get(api)
    if not model:
        raise ConfigError(f"--model (or DEMO_AGENT_MODEL) is required with --api {api}")
    base_url = base_url or API_DEFAULTS[api][0]
    if api == "anthropic":
        from langchain_anthropic import ChatAnthropic

        kwargs = {"base_url": base_url} if base_url else {}
        return ChatAnthropic(model=model, api_key=SecretStr(key), max_tokens=max_tokens, **kwargs)
    from langchain_openai import ChatOpenAI

    return ChatOpenAI(model=model, api_key=SecretStr(key), base_url=base_url, max_tokens=max_tokens)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    env = os.environ.get
    parser = argparse.ArgumentParser(
        prog="demo-agent",
        description=(
            "Give a model a task and the proveno-gateway's MCP tools, in LangChain's prebuilt "
            "agent graph. The model reads the `execute` description and writes Lua programs, "
            "possibly several, all run under one session. A successful run and a lint error go "
            f"back to the model, up to {MAX_LINT_STREAK} lint errors in a row; a failed run is "
            "never retried. Prints every message as it happens."
        ),
        epilog=(
            "Examples:\n"
            '  demo-agent "$(../run.sh --print-task)"\n'
            "      (Anthropic; key in ANTHROPIC_API_KEY)\n"
            '  demo-agent --api openai --model qwen/qwen3-coder "..."\n'
            "      (OpenRouter; key in OPENROUTER_API_KEY)\n\n"
            f"The gateway bearer token is read from {TOKEN_VAR}."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("task", help="the task, in natural language")
    parser.add_argument(
        "--api",
        choices=sorted(API_DEFAULTS),
        default=env("DEMO_AGENT_API", "anthropic"),
        help="`anthropic`, or `openai` for an OpenAI-compatible endpoint; default anthropic "
        "(DEMO_AGENT_API)",
    )
    parser.add_argument(
        "--model",
        default=env("DEMO_AGENT_MODEL"),
        help=f"model id as the endpoint names it; default {DEFAULT_MODELS['anthropic']} for "
        "anthropic, required for openai (DEMO_AGENT_MODEL)",
    )
    parser.add_argument(
        "--base-url",
        default=env("DEMO_AGENT_BASE_URL"),
        help="API base URL; default Anthropic's, or openrouter.ai/api/v1 (DEMO_AGENT_BASE_URL)",
    )
    parser.add_argument(
        "--api-key-var",
        default=env("DEMO_AGENT_API_KEY_VAR"),
        help="environment variable holding the API key; default ANTHROPIC_API_KEY or "
        "OPENROUTER_API_KEY (DEMO_AGENT_API_KEY_VAR)",
    )
    parser.add_argument("--max-tokens", type=int, default=16000, help="per reply; default 16000")
    parser.add_argument("--url", default=DEFAULT_URL, help=f"gateway MCP endpoint; {DEFAULT_URL}")
    parser.add_argument(
        "--max-runs",
        type=int,
        default=MAX_RUNS,
        help=f"most successful `execute` runs before the agent stops; default {MAX_RUNS}",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="print only the outcome and every run's trace_id, not the message flow",
    )
    parser.add_argument(
        "--result-file",
        type=Path,
        help="write the outcome, the session and every run (result, error, trace_id) here, as JSON",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    token = os.environ.get(TOKEN_VAR)
    if not token:
        print(f"error: {TOKEN_VAR} is not set; it is the gateway bearer token", file=sys.stderr)
        return 2
    try:
        model = build_model(args.api, args.model, args.base_url, args.api_key_var, args.max_tokens)
    except ConfigError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    # The gateway answers the session DELETE on close with 202, which the MCP
    # client logs as a failed termination; the session has ended either way.
    logging.getLogger("mcp.client.streamable_http").setLevel(logging.ERROR)
    if not args.quiet:
        print(f"[model {args.model or DEFAULT_MODELS[args.api]} via the {args.api} API]")
    if args.max_runs < 1:
        print("error: --max-runs must be at least 1", file=sys.stderr)
        return 2
    report = asyncio.run(
        run(
            model,
            args.task,
            args.url,
            token,
            sys.stdout,
            max_runs=args.max_runs,
            quiet=args.quiet,
        )
    )
    if args.result_file:
        args.result_file.write_text(json.dumps(report.to_json(), indent=2) + "\n")
    if not report.succeeded:
        print(f"demo-agent: stopped because {report.stop.value}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
