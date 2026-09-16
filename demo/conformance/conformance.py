#!/usr/bin/env python3
"""One-shot dialect conformance harness for proveno-gateway.

Measures whether a model can write a correct program for the gateway's Lua
dialect from the generated `execute` description alone. For each fixture and
each sample it gives the model only that description and the fixture's task,
lints the program with `proveno-gateway check`, resets the chain balances, runs
the program through `execute`, and checks the chain against the fixture.

This calls a paid model API, so every run costs money, and model output is not
deterministic, so two runs give different numbers. It is not part of
`make check` and must never run in CI.

Standard library only. See README.md in this directory.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEMO = HERE.parent
ROOT = DEMO.parent

GATEWAY_BIN = ROOT / "target/debug/proveno-gateway"
CLIENT_BIN = DEMO / "target/debug/demo-client"
MARKET_BIN = DEMO / "target/debug/demo-market"
WALLET_BIN = DEMO / "target/debug/demo-wallet"

PRINCIPAL = "demo-agent"
# Anvil's first two test accounts. The key is published in Anvil's own
# documentation and holds nothing outside a throwaway chain.
HOT = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
VAULT = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
ANVIL_TEST_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
MILLI_WEI = 10**15

OUTCOMES = ("correct", "wrong_result", "lint_error", "runtime_error")
OUTCOME_LABELS = {
    "correct": "compiled and correct",
    "wrong_result": "compiled but wrong result",
    "lint_error": "lint error",
    "runtime_error": "runtime error",
}

API_DEFAULTS = {
    "anthropic": ("https://api.anthropic.com", "ANTHROPIC_API_KEY"),
    "openai": ("https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
}

PROMPT = """\
You are writing a program for the `execute` tool of an MCP server. Below is \
that tool's description, exactly as the server publishes it, followed by a \
task. Write one program that performs the task when passed to `execute`.

Reply with the complete program in a single ```lua code block.

<tool_description>
{description}
</tool_description>

<task>
{task}
</task>
"""

RETRY_PROMPT = """\
`execute` rejected the program before running it:

{error}

Reply with the complete corrected program in a single ```lua code block.
"""


class HarnessError(Exception):
    """A failure of the harness or its world, not of the model's program."""


# ── model access ──────────────────────────────────────────────────────────────


@dataclass
class Model:
    api: str
    base_url: str
    key_var: str
    model: str
    max_tokens: int
    usage: dict = field(default_factory=lambda: {"input_tokens": 0, "output_tokens": 0, "calls": 0})

    def ask(self, messages: list[dict]) -> tuple[str, dict]:
        """Sends the conversation and returns the reply text and the assistant
        message to append if the conversation continues."""
        key = os.environ.get(self.key_var)
        if not key:
            raise HarnessError(f"{self.key_var} is not set; it holds the model API key")
        if self.api == "anthropic":
            url = self.base_url.rstrip("/") + "/v1/messages"
            headers = {"x-api-key": key, "anthropic-version": "2023-06-01"}
            body = {"model": self.model, "max_tokens": self.max_tokens, "messages": messages}
        else:
            url = self.base_url.rstrip("/") + "/chat/completions"
            headers = {"authorization": f"Bearer {key}"}
            body = {"model": self.model, "max_tokens": self.max_tokens, "messages": messages}
        reply = _post_json(url, headers, body)
        self.usage["calls"] += 1
        if self.api == "anthropic":
            usage = reply.get("usage", {})
            self.usage["input_tokens"] += usage.get("input_tokens", 0)
            self.usage["output_tokens"] += usage.get("output_tokens", 0)
            content = reply.get("content", [])
            text = "".join(b.get("text", "") for b in content if b.get("type") == "text")
            # Echo the whole content back, thinking blocks included, so a
            # follow-up turn is a faithful continuation.
            return text, {"role": "assistant", "content": content}
        usage = reply.get("usage") or {}
        self.usage["input_tokens"] += usage.get("prompt_tokens", 0)
        self.usage["output_tokens"] += usage.get("completion_tokens", 0)
        choices = reply.get("choices") or []
        if not choices:
            raise HarnessError(f"model reply has no choices: {json.dumps(reply)[:500]}")
        text = choices[0].get("message", {}).get("content") or ""
        return text, {"role": "assistant", "content": text}


def _post_json(url: str, headers: dict, body: dict) -> dict:
    data = json.dumps(body).encode()
    all_headers = {"content-type": "application/json", **headers}
    delay = 5.0
    for attempt in range(5):
        request = urllib.request.Request(url, data=data, headers=all_headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=600) as response:
                return json.load(response)
        except urllib.error.HTTPError as e:
            detail = e.read().decode(errors="replace")[:1000]
            retryable = e.code in (408, 409, 429, 500, 502, 503, 504, 529)
            if not retryable or attempt == 4:
                raise HarnessError(f"model API returned HTTP {e.code}: {detail}") from None
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            if attempt == 4:
                raise HarnessError(f"model API unreachable: {e}") from None
        time.sleep(delay)
        delay *= 2
    raise AssertionError("unreachable")


def extract_program(text: str) -> str:
    """The last ```lua block, else the last fenced block, else the whole reply."""
    blocks = re.findall(r"```([A-Za-z]*)[ \t]*\n(.*?)```", text, flags=re.S)
    lua = [body for lang, body in blocks if lang.lower() == "lua"]
    if lua:
        return lua[-1]
    if blocks:
        return blocks[-1][1]
    return text


# ── the world: anvil, demo-market, the gateway ────────────────────────────────


def port_free(port: int) -> bool:
    with socket.socket() as s:
        return s.connect_ex(("127.0.0.1", port)) != 0


def wait_for_port(port: int, proc: subprocess.Popen, log: Path, what: str) -> None:
    for _ in range(300):
        if proc.poll() is not None:
            break
        if not port_free(port):
            return
        time.sleep(0.1)
    raise HarnessError(f"{what} did not start on port {port}; see {log}")


class World:
    def __init__(self, out: Path, ports: dict):
        self.out = out
        self.ports = ports
        self.rpc = f"http://127.0.0.1:{ports['anvil']}"
        self.gateway_url = f"http://127.0.0.1:{ports['gateway']}/mcp"
        self.config = out / "gateway.toml"
        self.anvil = self.market = self.gateway = None
        self.env = {k: v for k, v in os.environ.items() if not _secret_var(k)}
        self.env.update(
            PROVENO_SIGNING_KEY=secrets.token_hex(32),
            DEMO_AGENT_TOKEN=secrets.token_hex(16),
            MARKET_KEY=(market_key := secrets.token_hex(16)),
            MARKET_TOKEN=market_key,
            WALLET_PRIVATE_KEY=ANVIL_TEST_KEY,
        )

    def write_config(self) -> None:
        policy = self.out / "policy.toml"
        shutil.copyfile(DEMO / "policy.toml", policy)
        self.config.write_text(
            f"""\
# Written by demo/conformance/conformance.py for this run only.
[server]
listen = "127.0.0.1:{self.ports['gateway']}"
signing_key = "env:PROVENO_SIGNING_KEY"

[vm]
gas_limit = 2_000_000
memory_limit_bytes = 16_777_216
max_tool_calls = 64

[[downstream]]
name = "wallet"
transport = "stdio"
command = "{WALLET_BIN} --rpc-url {self.rpc}"
credential = "env:WALLET_PRIVATE_KEY"

[[downstream]]
name = "market"
transport = "http"
url = "http://127.0.0.1:{self.ports['market']}/mcp"
credential = "env:MARKET_KEY"

[policy]
file = "{policy}"

[store]
dir = "{self.out / 'traces'}"

[principals.{PRINCIPAL}]
token = "env:DEMO_AGENT_TOKEN"
"""
        )

    def _spawn(self, name: str, args: list[str], port: int) -> subprocess.Popen:
        log = self.out / "logs" / f"{name}.log"
        log.parent.mkdir(parents=True, exist_ok=True)
        handle = open(log, "a")
        proc = subprocess.Popen(
            args, stdout=handle, stderr=subprocess.STDOUT, env=self.env, cwd=DEMO,
            start_new_session=True,
        )
        wait_for_port(port, proc, log, name)
        return proc

    def start_chain(self) -> None:
        self.anvil = self._spawn(
            "anvil", ["anvil", "--port", str(self.ports["anvil"]), "--silent"], self.ports["anvil"]
        )

    def start_fixture(self, fixture: dict) -> None:
        """(Re)starts demo-market on the fixture's prices, and the gateway."""
        self.stop_fixture()
        prices = self.out / "fixtures" / f"{fixture['name']}.prices.json"
        prices.parent.mkdir(parents=True, exist_ok=True)
        prices.write_text(json.dumps(fixture["prices"], indent=2))
        self.market = self._spawn(
            "market",
            [str(MARKET_BIN), "--listen", f"127.0.0.1:{self.ports['market']}", "--fixture", str(prices)],
            self.ports["market"],
        )
        self.gateway = self._spawn(
            "gateway", [str(GATEWAY_BIN), "serve", "--config", str(self.config)], self.ports["gateway"]
        )

    def stop_fixture(self) -> None:
        _stop(self.gateway, signal.SIGINT)
        _stop(self.market, signal.SIGTERM)
        self.gateway = self.market = None

    def stop(self) -> None:
        self.stop_fixture()
        _stop(self.anvil, signal.SIGTERM)
        self.anvil = None

    def set_balances(self, balances_milli: dict) -> None:
        for address, key in ((HOT, "hot"), (VAULT, "vault")):
            wei = balances_milli[key] * MILLI_WEI
            self._run(["cast", "rpc", "anvil_setBalance", address, hex(wei), "--rpc-url", self.rpc])

    def balance_wei(self, address: str) -> int:
        return int(self._run(["cast", "balance", address, "--rpc-url", self.rpc]).strip())

    def description(self) -> str:
        return self._run([str(CLIENT_BIN), "--url", self.gateway_url, "description"])

    def check(self, program: Path) -> tuple[bool, str]:
        proc = subprocess.run(
            [str(GATEWAY_BIN), "check", "--config", str(self.config), "--principal", PRINCIPAL, str(program)],
            capture_output=True, text=True, env=self.env, cwd=DEMO,
        )
        if proc.returncode == 0:
            return True, proc.stdout.strip()
        if proc.returncode == 1:
            return False, proc.stdout.strip()
        raise HarnessError(f"`proveno-gateway check` could not run: {proc.stderr.strip()}")

    def execute(self, program: Path, task: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [str(CLIENT_BIN), "--url", self.gateway_url, "execute", str(program), "--request", task],
            capture_output=True, text=True, env=self.env, cwd=DEMO, timeout=300,
        )

    def _run(self, args: list[str]) -> str:
        proc = subprocess.run(args, capture_output=True, text=True, env=self.env, cwd=DEMO)
        if proc.returncode != 0:
            raise HarnessError(f"`{' '.join(args[:3])}` failed: {proc.stderr.strip()}")
        return proc.stdout


def _secret_var(name: str) -> bool:
    return bool(re.search(r"API_KEY|_TOKEN$|SECRET", name))


def _stop(proc: subprocess.Popen | None, sig: int) -> None:
    if proc is None or proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, sig)
        proc.wait(timeout=10)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()


# ── one sample ────────────────────────────────────────────────────────────────


def normalize_error(message: str) -> str:
    """The cause of a failure, with the parts that vary between samples removed,
    so identical causes group together."""
    first = message.strip().splitlines()[0] if message.strip() else "(empty)"
    first = re.sub(r"^(Error:\s*)?line \d+:\s*", "", first)
    first = re.sub(r"0x[0-9a-fA-F]{6,}", "0x…", first)
    first = re.sub(r"\b\d+\b", "N", first)
    return first[:160]


def run_attempt(world: World, fixture: dict, program_text: str, directory: Path) -> dict:
    directory.mkdir(parents=True, exist_ok=True)
    program = directory / "program.lua"
    program.write_text(program_text)

    ok, lint = world.check(program)
    (directory / "check.txt").write_text(lint + "\n")
    if not ok:
        return {"outcome": "lint_error", "error": lint, "cause": normalize_error(lint)}

    world.set_balances(fixture["balances_milli"])
    hot_before, vault_before = world.balance_wei(HOT), world.balance_wei(VAULT)
    proc = world.execute(program, fixture["task"])
    (directory / "execute.stdout.txt").write_text(proc.stdout)
    (directory / "execute.stderr.txt").write_text(proc.stderr)
    hot_sent = (hot_before - world.balance_wei(HOT)) // MILLI_WEI
    vault_gain = (world.balance_wei(VAULT) - vault_before) // MILLI_WEI
    observed = {"hot_sent_milli": hot_sent, "vault_gain_milli": vault_gain}

    if proc.returncode != 0:
        error = proc.stderr.strip() or proc.stdout.strip()
        # `check` passed, so a rejection here is a failed run.
        return {"outcome": "runtime_error", "error": error, "cause": normalize_error(error), **observed}
    response = json.loads(proc.stdout)
    status = response.get("status") or {}
    if status.get("type") != "ok":
        error = f"{status.get('kind', 'error')}: {status.get('message', json.dumps(status))}"
        return {
            "outcome": "runtime_error", "error": error, "cause": normalize_error(error),
            "trace_id": response.get("trace_id"), **observed,
        }
    expected = fixture["expect"]["transfer_milli"]
    result = {"trace_id": response.get("trace_id"), "result": response.get("result"), **observed}
    if hot_sent == expected and vault_gain == expected:
        return {"outcome": "correct", **result}
    error = f"expected a transfer of {expected} milli-ETH, the vault gained {vault_gain}"
    return {"outcome": "wrong_result", "error": error, "cause": normalize_error(error), **result}


def run_sample(world: World, model: Model, fixture: dict, description: str, directory: Path) -> dict:
    directory.mkdir(parents=True, exist_ok=True)
    prompt = PROMPT.format(description=description.strip(), task=fixture["task"])
    messages = [{"role": "user", "content": prompt}]

    reply, assistant = model.ask(messages)
    (directory / "attempt-1").mkdir(exist_ok=True)
    (directory / "attempt-1" / "reply.md").write_text(reply)
    first = run_attempt(world, fixture, extract_program(reply), directory / "attempt-1")
    sample = {"first": first}

    if first["outcome"] == "lint_error":
        messages += [assistant, {"role": "user", "content": RETRY_PROMPT.format(error=first["error"])}]
        reply, _ = model.ask(messages)
        (directory / "attempt-2").mkdir(exist_ok=True)
        (directory / "attempt-2" / "reply.md").write_text(reply)
        sample["second"] = run_attempt(world, fixture, extract_program(reply), directory / "attempt-2")

    (directory / "outcome.json").write_text(json.dumps(sample, indent=2) + "\n")
    return sample


# ── summary ───────────────────────────────────────────────────────────────────


def summarize(fixture_name: str, samples: list[dict]) -> dict:
    n = len(samples)
    first = [s["first"]["outcome"] for s in samples]
    seconds = [s["second"] for s in samples if "second" in s]
    first_causes: dict[str, int] = {}
    for s in samples:
        if s["first"]["outcome"] != "correct":
            key = f"{OUTCOME_LABELS[s['first']['outcome']]}: {s['first']['cause']}"
            first_causes[key] = first_causes.get(key, 0) + 1
    second_causes: dict[str, int] = {}
    for s in seconds:
        if s["outcome"] != "correct":
            key = f"{OUTCOME_LABELS[s['outcome']]}: {s['cause']}"
            second_causes[key] = second_causes.get(key, 0) + 1
    fixed = sum(1 for s in seconds if s["outcome"] == "correct")
    return {
        "fixture": fixture_name,
        "samples": n,
        "one_shot_correct": first.count("correct"),
        "one_round_trip_correct": first.count("correct") + fixed,
        "lint_errors_fed_back": len(seconds),
        "fixed_after_feedback": fixed,
        "first_attempt": {o: first.count(o) for o in OUTCOMES},
        "failure_causes": dict(sorted(first_causes.items(), key=lambda kv: (-kv[1], kv[0]))),
        "second_attempt_failure_causes": dict(sorted(second_causes.items(), key=lambda kv: (-kv[1], kv[0]))),
    }


def render(summary: dict) -> str:
    n = summary["samples"]
    lines = [
        f"fixture {summary['fixture']}",
        f"  one-shot:       {summary['one_shot_correct']}/{n} compiled and correct",
        f"  one round trip: {summary['one_round_trip_correct']}/{n} "
        f"(lint errors fed back {summary['lint_errors_fed_back']}, fixed {summary['fixed_after_feedback']})",
        "  first attempt:  " + ", ".join(
            f"{OUTCOME_LABELS[o]} {summary['first_attempt'][o]}" for o in OUTCOMES
        ),
    ]
    if summary["failure_causes"]:
        lines.append("  failures by cause, first attempt:")
        lines += [f"    {count}x {cause}" for cause, count in summary["failure_causes"].items()]
    if summary["second_attempt_failure_causes"]:
        lines.append("  failures by cause, after one lint round trip:")
        lines += [f"    {count}x {cause}" for cause, count in summary["second_attempt_failure_causes"].items()]
    return "\n".join(lines)


# ── main ──────────────────────────────────────────────────────────────────────


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "One-shot dialect conformance harness. Asks a model for a Lua program given only the "
            "gateway's `execute` description and a task, lints it, runs it through a private "
            "gateway on a throwaway Anvil chain, and reports the one-shot and one-round-trip rates."
        ),
        epilog=(
            "COST AND DETERMINISM: every sample calls a paid model API, so a run costs money. It "
            "needs an API key in the environment variable named by --api-key-var. Model output is "
            "not deterministic, so two runs give different numbers. This harness is not part of "
            "`make check` and must not run in CI.\n\n"
            "Examples:\n"
            "  conformance.py --model claude-haiku-4-5 --samples 4\n"
            "  conformance.py --api openai --model qwen/qwen3-coder --samples 4\n"
            "      (OpenRouter by default; key in OPENROUTER_API_KEY)\n"
            "  conformance.py --api openai --base-url http://localhost:11434/v1 "
            "--api-key-var OLLAMA_KEY --model qwen3:32b"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--model", required=True, help="model id, as the endpoint names it")
    parser.add_argument(
        "--api", choices=sorted(API_DEFAULTS), default="anthropic",
        help="request shape: `anthropic` (Messages API) or `openai` (chat completions, as served "
             "by OpenRouter and most open-model hosts); default anthropic",
    )
    parser.add_argument("--base-url", help="API base URL; default api.anthropic.com or openrouter.ai/api/v1")
    parser.add_argument(
        "--api-key-var",
        help="environment variable holding the API key; default ANTHROPIC_API_KEY or OPENROUTER_API_KEY",
    )
    parser.add_argument("--samples", type=int, default=4, help="samples per fixture; default 4")
    parser.add_argument(
        "--fixture", action="append", dest="fixtures",
        help="fixture name to run (repeatable); default all fixtures in fixtures/",
    )
    parser.add_argument("--max-tokens", type=int, default=16000, help="per reply; default 16000")
    parser.add_argument("--out", type=Path, help="output directory; default demo/conformance/out/<time>-<model>")
    parser.add_argument("--anvil-port", type=int, default=18545)
    parser.add_argument("--market-port", type=int, default=18081)
    parser.add_argument("--gateway-port", type=int, default=17777)
    parser.add_argument("--no-build", action="store_true", help="skip `cargo build`")
    return parser.parse_args()


def load_fixtures(names: list[str] | None) -> list[dict]:
    available = {p.stem: p for p in sorted((HERE / "fixtures").glob("*.json"))}
    chosen = names or list(available)
    unknown = [n for n in chosen if n not in available]
    if unknown:
        raise HarnessError(f"unknown fixture {unknown}; available: {sorted(available)}")
    return [json.loads(available[n].read_text()) for n in chosen]


def main() -> int:
    args = parse_args()
    base_url, key_var = API_DEFAULTS[args.api]
    model = Model(
        api=args.api, base_url=args.base_url or base_url, key_var=args.api_key_var or key_var,
        model=args.model, max_tokens=args.max_tokens,
    )
    if not os.environ.get(model.key_var):
        print(f"error: {model.key_var} is not set; it holds the model API key", file=sys.stderr)
        return 2
    for tool in ("anvil", "cast", "cargo"):
        if shutil.which(tool) is None:
            print(f"error: `{tool}` is not on PATH", file=sys.stderr)
            return 2
    fixtures = load_fixtures(args.fixtures)
    ports = {"anvil": args.anvil_port, "market": args.market_port, "gateway": args.gateway_port}
    busy = [f"{name} {port}" for name, port in ports.items() if not port_free(port)]
    if busy:
        print(f"error: ports in use: {', '.join(busy)}; pick others with --*-port", file=sys.stderr)
        return 2

    if not args.no_build:
        subprocess.run(["cargo", "build", "--quiet"], cwd=ROOT, check=True)
        subprocess.run(["cargo", "build", "--quiet", "--workspace"], cwd=DEMO, check=True)

    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    slug = re.sub(r"[^A-Za-z0-9._-]+", "_", args.model)
    out = (args.out or HERE / "out" / f"{stamp}-{slug}").resolve()
    out.mkdir(parents=True, exist_ok=True)

    world = World(out, ports)
    # A custom key variable name may not look secret to `_secret_var`.
    world.env.pop(model.key_var, None)
    world.write_config()
    summaries = []
    started = time.monotonic()
    try:
        world.start_chain()
        description = None
        for fixture in fixtures:
            world.start_fixture(fixture)
            if description is None:
                description = world.description()
                (out / "description.md").write_text(description)
            print(f"== {fixture['name']}: {args.samples} samples of {args.model}", flush=True)
            samples = []
            for i in range(1, args.samples + 1):
                sample = run_sample(
                    world, model, fixture, description, out / fixture["name"] / f"sample-{i:02d}"
                )
                line = f"   sample {i}: {OUTCOME_LABELS[sample['first']['outcome']]}"
                if "cause" in sample["first"]:
                    line += f" ({sample['first']['cause']})"
                if "second" in sample:
                    line += f"; after feedback: {OUTCOME_LABELS[sample['second']['outcome']]}"
                print(line, flush=True)
                samples.append(sample)
            summaries.append(summarize(fixture["name"], samples))
    except HarnessError as e:
        print(f"harness error: {e}", file=sys.stderr)
        return 2
    finally:
        world.stop()

    report = {
        "date": stamp,
        "model": args.model,
        "api": args.api,
        "base_url": model.base_url,
        "samples": args.samples,
        "description_sha256": hashlib.sha256(description.encode()).hexdigest(),
        "usage": model.usage,
        "seconds": round(time.monotonic() - started),
        "fixtures": summaries,
    }
    (out / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    text = "\n\n".join(
        [
            f"model {args.model} via {model.base_url}, {args.samples} samples per fixture, {stamp}",
            f"description sha256 {report['description_sha256']}",
            *(render(s) for s in summaries),
            f"model calls {model.usage['calls']}, input tokens {model.usage['input_tokens']}, "
            f"output tokens {model.usage['output_tokens']}",
            f"every program, reply and error is under {out}",
        ]
    )
    (out / "summary.txt").write_text(text + "\n")
    print("\n" + text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
