# One-shot dialect conformance harness

The gateway rests on one claim: an agent reads the generated `execute`
description and writes a working program. The tests in this repository use
programs written by hand, by someone who knows the dialect, so nothing there
tests the claim. This harness does.

**It costs money and it is not deterministic.** Every sample calls a paid model
API, and two runs of the same model give different numbers. It is not part of
`make check` and must never run in CI. Run it by hand when the description
changes, and compare the numbers and the failure grouping with the last run.

## What it does

For each fixture, and each of N samples:

1. Starts a private world: Anvil, `demo-market` on the fixture's prices, and a
   gateway with the demo policy, on ports that do not clash with `demo/run.sh`.
2. Captures `execute`'s description with `demo-client description`.
3. Gives the model **only** that description and the fixture's task, and asks
   for one Lua program. No repository, no examples beyond what the description
   holds, one shot.
4. Lints the program with `proveno-gateway check`.
5. If it lints, resets both balances, runs it through `execute`, and reads the
   chain. The outcome is one of:
   - **compiled and correct**: the run succeeded and the chain moved exactly as
     the fixture expects;
   - **compiled but wrong result**: the run succeeded, the chain did not;
   - **lint error**: `check` rejected it, so it never ran;
   - **runtime error**: it linted but the run failed.
6. On a lint error, feeds the error back once and scores the second attempt the
   same way. That gives the one-round-trip rate, the number behind the spec's
   "the model corrects and resubmits in one round trip".

The check is on the chain, not on the program's return value, because the model
chooses the shape of its result. A fixture names the transfer that must have
happened, in milli-ETH; `0` means none may.

## Fixtures

| Fixture | Situation | Expected |
|---|---|---|
| `rebalance` | The demo task: price moved 203 bps, hot wallet 20 over target | a transfer of 20 milli-ETH |
| `hold` | Price moved 40 bps | no transfer |
| `refused` | Hot wallet 100 over target, above `amount_max = 50` | no transfer, and the run must catch the refusal |

A fixture is a JSON file in `fixtures/`: `task`, `prices` (the `demo-market`
fixture), `balances_milli` for `hot` and `vault`, and `expect.transfer_milli`.

## Run it

Needs `anvil` and `cast` (Foundry), `cargo` and Python 3.9 or later. Standard
library only.

```bash
# Anthropic's API; key in ANTHROPIC_API_KEY
demo/conformance/conformance.py --model claude-haiku-4-5 --samples 4
demo/conformance/conformance.py --model claude-opus-5 --samples 4 --fixture rebalance

# OpenRouter, or any OpenAI-compatible endpoint; key in OPENROUTER_API_KEY
demo/conformance/conformance.py --api openai --model qwen/qwen3-coder --samples 4

# A local or other endpoint
demo/conformance/conformance.py --api openai --base-url http://localhost:11434/v1 \
    --api-key-var LOCAL_KEY --model qwen3:32b
```

`--api` picks the request shape (`anthropic` for the Messages API, `openai` for
chat completions), `--base-url` the endpoint, `--api-key-var` the environment
variable holding the key, and `--model` the model id as that endpoint names it.
Switching provider is configuration, not code. The key is read from the
environment, never printed, and never passed to the gateway or the demo
processes.

It builds the gateway and the demo binaries first (`--no-build` skips that),
and stops every process it started when it exits.

## Output

Everything lands in `demo/conformance/out/<UTC time>-<model>/` (ignored by git;
`--out` overrides):

```
description.md              the description the model was given
gateway.toml, policy.toml   this run's gateway config
summary.txt, summary.json   the numbers
logs/                       anvil, demo-market and gateway output
traces/                     the signed traces of every run
<fixture>/sample-NN/
  outcome.json              outcome, cause, error, observed transfer
  attempt-1/reply.md        the model's whole reply
  attempt-1/program.lua     the program taken from it
  attempt-1/check.txt       the lint result
  attempt-1/execute.*.txt   the execute result, if it linted
  attempt-2/...             the same, after one lint error was fed back
```

The summary prints, per fixture, the one-shot rate, the one-round-trip rate,
the count of each outcome, and the failures grouped by cause. A cause is the
error's first line with line numbers and addresses masked, so the same mistake
in two samples groups together, and a change to the description shows up as a
change in the grouping. `summary.json` also records the description's SHA-256,
so two runs can be checked to have measured the same text.
