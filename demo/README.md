# The proveno-gateway pilot demo

This is the demo of spec section 5: an agent rebalances a wallet through the
gateway, the run replays with the world switched off, and a policy change is
enforced at the call.

It runs entirely on localhost against a throwaway Anvil chain. The transactions
are real signed transactions; the chain is not.

## Prerequisites

- A Rust toolchain (`cargo`).
- [Foundry](https://getfoundry.sh): `anvil` and `cast` on `PATH`.
- `jq`.

Ports 8545 (Anvil), 8081 (the price server) and 7777 (the gateway) must be free.

## Run it

```bash
./demo/run.sh
```

One command. It builds the gateway and the three demo binaries, starts
everything, runs the four steps, and stops every process it started. It takes a
few minutes on a cold build and a few seconds afterwards.

## What each step proves

The prototype has to show three things (spec section 1): an agent can do real
work this way, a run can be reproduced exactly from its record, and a policy
change is enforced at the call with the refusal in the record.

**Step 1: an agent can do real work.** The task is "rebalance to 60/40 if the
price has moved more than 2%". `rebalance.lua` reads the ETH/USD price and the
two balances, works out that the price moved 203 basis points and that the hot
wallet is 20 milli-ETH over its 60% target, and transfers 20 milli-ETH to the
vault. The step prints the generated `wallet.transfer` signature from the tool
description the model was given, the program's result, the transaction as `cast
tx` sees it, and the signed trace: header, every call with its policy decision
and provenance tag, and a footer with the output, gas and memory.

**Step 2: a run can be reproduced exactly from its record.** The chain, the
price server and the wallet server are stopped, and `proveno-gateway replay`
runs the program again from the trace alone. It prints `replay matched`, the
same output, the same `gas_used` and the same `memory_used`, with no network
access at all.

**Step 3: a policy change is enforced at the call.** `policy.toml` drops to
`amount_max = 10` and the same program runs again on a fresh chain. The transfer
is refused before it is dispatched, so nothing reaches the chain. The program
catches the error with `pcall` and reports `action = "refused"`, and the trace
records the call with `denied_by_policy` and the reason.

**Step 4: the refusal holds for the primitive too.** `wallet.transfer` comes off
the allow-list entirely. It disappears from the generated tool API, so a model
reading the description never sees it. `direct_transfer.lua` calls it through
the `tool.call` primitive anyway, and the policy refuses it at run time, in the
trace.

## What is in here

| File | What it is |
|---|---|
| `wallet/` | `demo-wallet`, a stdio MCP server that signs real transactions with `alloy` |
| `market/` | `demo-market`, an http MCP server serving prices from `prices.json` |
| `client/` | `demo-client`, a tiny MCP client standing in for an agent |
| `gateway.toml` | The gateway config: two downstreams, the policy file, the trace store |
| `policy.toml` | The allow-list and `amount_max`; `run.sh` edits and restores it |
| `prices.json` | The price fixture, so every run sees the same market |
| `rebalance.lua` | The program the agent wrote |
| `direct_transfer.lua` | Step 4's program, calling the tool through the primitive |
| `run.sh` | The four steps |

`demo/` is a Cargo workspace of its own, so the gateway's `make check` does not
build it. Build it with `(cd demo && cargo build --workspace)`.

Amounts are integers in milli-ETH, because the VM has integers and no floats.
Prices are decimals in the fixture and reach the program as decimal strings, so
`rebalance.lua` computes the move in basis points from the digits.

## Secrets

Every secret in `gateway.toml` is an `env:NAME` reference, so no credential is
in this repository.

- The signing key, the agent's bearer token and the market key are generated
  fresh by `run.sh` on each run. A trace is signed with the key of the run that
  produced it, so `run.sh` starts from an empty `traces/` directory; traces from
  an earlier run will not verify against a later run's key.
- The wallet's private key is Anvil's first test account,
  `0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80`, which is
  published in Anvil's own documentation and holds nothing outside a local test
  chain. No real key is ever involved.

`run.sh` sets that key only on the gateway process, as the `wallet` downstream's
`credential`. The gateway spawns `demo-wallet` with an environment cleared down
to `PATH`, `HOME` and `WALLET_PRIVATE_KEY`. The key is never a command line
argument, never in a config file, and never visible to the program or the model.
That is why `demo-wallet` takes its chain endpoint as `--rpc-url`: it has no
other environment to read it from.

## Driving step 1 from a live agent

Step 1 needs no `demo-client`; any MCP client can do it. Start the world by
hand, with secrets you choose:

```bash
cd demo
export PROVENO_SIGNING_KEY=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
export DEMO_AGENT_TOKEN=demo-agent-token
export MARKET_KEY=market-key
export MARKET_TOKEN=$MARKET_KEY

anvil --port 8545 --silent &
cast rpc anvil_setBalance 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
    "$(cast to-hex 620000000000000000)" --rpc-url http://127.0.0.1:8545
cast rpc anvil_setBalance 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
    "$(cast to-hex 380000000000000000)" --rpc-url http://127.0.0.1:8545

cargo build --workspace
./target/debug/demo-market --listen 127.0.0.1:8081 --fixture prices.json &

(cd .. && cargo build)
WALLET_PRIVATE_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
    ../target/debug/proveno-gateway serve --config gateway.toml
```

Then point Claude Code at it:

```bash
claude mcp add --transport http proveno-gateway http://127.0.0.1:7777/mcp \
    --header "Authorization: Bearer $DEMO_AGENT_TOKEN"
```

Ask it to rebalance to 60/40 if the price has moved more than 2%. It will read
the dialect and the tool API from `execute`'s description, write its own
program, and run it. `proveno://lua-guide` is available as a resource and as a
prompt. Keep the gateway on localhost: the MCP server rejects requests addressed
to anything else.
