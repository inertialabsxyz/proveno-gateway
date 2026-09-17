#!/usr/bin/env bash
#
# The proveno-gateway pilot demo: the four steps of spec section 5.
#
# 1. An agent does real work: a Lua program reads a price and two balances and
#    makes a real signed transfer, recorded in a signed trace. With a model API
#    key in the environment, a LangChain agent (demo/agent) writes that program;
#    without one, the script runs rebalance.lua, a program an agent wrote
#    earlier.
# 2. That run replays bit-for-bit with the chain and both tool servers stopped.
# 3. A tighter policy refuses the transfer at the call, and records the refusal.
# 4. A tool off the allow-list is absent from the generated API and refused at
#    run time even when called through the `tool.call` primitive.
#
# Everything runs on localhost against a throwaway Anvil chain.

set -euo pipefail

cd "$(dirname "$0")"

GATEWAY=../target/debug/proveno-gateway
CLIENT=./target/debug/demo-client
MARKET=./target/debug/demo-market

RPC=http://127.0.0.1:8545
GATEWAY_PORT=7777
MARKET_PORT=8081

# Anvil's first two test accounts. The key below is published in Anvil's own
# documentation and holds nothing outside this throwaway chain.
HOT=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
VAULT=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
ANVIL_TEST_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80

# 0.62 and 0.38 ETH, in wei: 620 and 380 milli-ETH, so 60:40 is 20 milli-ETH
# away.
HOT_WEI=620000000000000000
VAULT_WEI=380000000000000000

REQUEST="rebalance to 60/40 if the price has moved more than 2%"

ANVIL_PID=""
MARKET_PID=""
GATEWAY_PID=""

banner() {
    printf '\n========================================================\n'
    printf '%s\n' "$1"
    printf '========================================================\n\n'
}

say() { printf '\n--- %s\n\n' "$1"; }

fail() {
    printf 'demo failed: %s\n' "$1" >&2
    exit 1
}

# ── prerequisites ─────────────────────────────────────────────────────────────

for tool in anvil cast cargo jq; do
    command -v "$tool" > /dev/null 2>&1 || fail "\`$tool\` is not on PATH"
done

# ── the model, if there is a key ──────────────────────────────────────────────

# The same configuration demo-agent reads: DEMO_AGENT_API picks `anthropic` or
# `openai` (any OpenAI-compatible endpoint, OpenRouter by default), and
# DEMO_AGENT_API_KEY_VAR names the variable holding the key. DEMO_AGENT_MODEL
# and DEMO_AGENT_BASE_URL pass through to demo-agent untouched.
AGENT_API=${DEMO_AGENT_API:-anthropic}
case "$AGENT_API" in
    anthropic) AGENT_KEY_VAR=${DEMO_AGENT_API_KEY_VAR:-ANTHROPIC_API_KEY} ;;
    openai) AGENT_KEY_VAR=${DEMO_AGENT_API_KEY_VAR:-OPENROUTER_API_KEY} ;;
    *) fail "DEMO_AGENT_API must be \`anthropic\` or \`openai\`, not \`$AGENT_API\`" ;;
esac
[[ "$AGENT_KEY_VAR" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] \
    || fail "DEMO_AGENT_API_KEY_VAR is not a variable name: \`$AGENT_KEY_VAR\`"
AGENT_KEY=${!AGENT_KEY_VAR:-}

# The key goes to demo-agent and nothing else: not to cargo, anvil, demo-market
# or the gateway. Un-exporting keeps the value in this shell only; `run_agent`
# exports it inside the agent's own subshell, so it is never an argument.
export -n ANTHROPIC_API_KEY OPENROUTER_API_KEY "$AGENT_KEY_VAR"

if [ -n "$AGENT_KEY" ]; then
    command -v uv > /dev/null 2>&1 || fail "$AGENT_KEY_VAR is set, so step 1 uses demo/agent, which needs \`uv\` on PATH"
fi

LOG_DIR=$(mktemp -d)

# ── processes ─────────────────────────────────────────────────────────────────

# stop <pid> [signal]: signal the process, wait for it to go, then insist.
stop() {
    local pid=$1 signal=${2:-TERM} waited=0
    [ -n "$pid" ] || return 0
    kill -"$signal" "$pid" 2> /dev/null || return 0
    while kill -0 "$pid" 2> /dev/null && [ "$waited" -lt 100 ]; do
        sleep 0.1
        waited=$((waited + 1))
    done
    kill -KILL "$pid" 2> /dev/null || true
    wait "$pid" 2> /dev/null || true
}

# The gateway owns the wallet server as a stdio child, so SIGINT to the gateway
# shuts both down.
stop_gateway() {
    stop "$GATEWAY_PID" INT
    GATEWAY_PID=""
}

stop_world() {
    stop_gateway
    stop "$MARKET_PID"
    MARKET_PID=""
    stop "$ANVIL_PID"
    ANVIL_PID=""
}

cleanup() {
    trap - EXIT INT TERM
    set +e
    stop_world
    if [ -f policy.toml.orig ]; then
        mv -f policy.toml.orig policy.toml
    fi
    rm -rf "$LOG_DIR"
}
trap cleanup EXIT INT TERM

wait_for_port() {
    local port=$1 waited=0
    while [ "$waited" -lt 200 ]; do
        if (exec 3<> "/dev/tcp/127.0.0.1/$port") 2> /dev/null; then
            exec 3<&- 3>&-
            return 0
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    return 1
}

start_chain() {
    anvil --port 8545 --silent > "$LOG_DIR/anvil.log" 2>&1 &
    ANVIL_PID=$!
    wait_for_port 8545 || { cat "$LOG_DIR/anvil.log"; fail "anvil did not start"; }
    cast rpc anvil_setBalance "$HOT" "$(cast to-hex "$HOT_WEI")" --rpc-url "$RPC" > /dev/null
    cast rpc anvil_setBalance "$VAULT" "$(cast to-hex "$VAULT_WEI")" --rpc-url "$RPC" > /dev/null
    # anvil_setBalance changes state without a block, so the latest block's state
    # root would not commit to these balances. Mine one so that a balance read's
    # `onchain` tag names a block whose state actually holds them.
    cast rpc anvil_mine --rpc-url "$RPC" > /dev/null
}

start_market() {
    "$MARKET" --listen "127.0.0.1:$MARKET_PORT" --fixture prices.json \
        > "$LOG_DIR/market.log" 2>&1 &
    MARKET_PID=$!
    wait_for_port "$MARKET_PORT" || { cat "$LOG_DIR/market.log"; fail "demo-market did not start"; }
}

# The wallet's key reaches demo-wallet only as this downstream's credential,
# injected into the child's environment by the gateway. It is never an argument
# and never in a config file.
start_gateway() {
    WALLET_PRIVATE_KEY="$ANVIL_TEST_KEY" "$GATEWAY" serve --config gateway.toml \
        > "$LOG_DIR/gateway.log" 2>&1 &
    GATEWAY_PID=$!
    wait_for_port "$GATEWAY_PORT" || { cat "$LOG_DIR/gateway.log"; fail "the gateway did not start"; }
}

start_world() {
    start_chain
    start_market
    start_gateway
}

# run_agent <result file>: the model writes and runs the program. demo-agent
# prints the program, the tool result and the trace_id, and writes the result
# to the file for the steps that follow.
run_agent() {
    (
        export "$AGENT_KEY_VAR=$AGENT_KEY"
        exec uv run --quiet --locked --project agent demo-agent \
            --api "$AGENT_API" --api-key-var "$AGENT_KEY_VAR" \
            --url "http://127.0.0.1:$GATEWAY_PORT/mcp" --result-file "$1" "$REQUEST"
    )
}

trace_file() { echo "traces/traces/$1.json"; }

# ── secrets, generated per run ────────────────────────────────────────────────

random_hex() { od -An -tx1 -N"$1" /dev/urandom | tr -d ' \n'; }

PROVENO_SIGNING_KEY=$(random_hex 32)
DEMO_AGENT_TOKEN=$(random_hex 16)
MARKET_KEY=$(random_hex 16)
export PROVENO_SIGNING_KEY DEMO_AGENT_TOKEN MARKET_KEY
# The same value the gateway injects as this downstream's bearer token.
export MARKET_TOKEN="$MARKET_KEY"

# ── build ─────────────────────────────────────────────────────────────────────

banner "proveno-gateway demo: building"
(cd .. && cargo build)
cargo build --workspace

# A previous interrupted run may have left an edited policy behind.
if [ -f policy.toml.orig ]; then
    mv -f policy.toml.orig policy.toml
fi
cp policy.toml policy.toml.orig
# The trace store is signed with this run's key, so start it empty.
rm -rf traces

# ── step 1 ────────────────────────────────────────────────────────────────────

banner "Step 1: an agent does real work, and the run is recorded"

start_world

say "The task: $REQUEST"

if [ -n "$AGENT_KEY" ]; then
    say "Step 1 path: $AGENT_KEY_VAR is set, so a live model writes the program (demo/agent, $AGENT_API API)"

    say "The tool API the gateway generated for this principal, in the \`execute\` description"
    "$CLIENT" description | grep -B1 'wallet.transfer{'

    say "The agent reads the description, writes its own program and runs it"
    run_agent "$LOG_DIR/agent-result.json" || fail "the agent did not complete a successful run"
    result=$(cat "$LOG_DIR/agent-result.json")
else
    say "Step 1 path: $AGENT_KEY_VAR is empty or unset, so no model runs; using the recorded program rebalance.lua"

    say "A program an agent wrote earlier (rebalance.lua)"
    cat rebalance.lua

    say "The tool API the gateway generated for this principal, in the \`execute\` description"
    "$CLIENT" description | grep -B1 'wallet.transfer{'

    say "Executing it through the gateway"
    result=$("$CLIENT" execute rebalance.lua --request "$REQUEST")
    echo "$result"
fi

trace_id=$(printf '%s' "$result" | jq -r .trace_id)
# The transfer is read from the trace, not from the program's return value,
# whose shape a model chooses.
tx_hashes=$(jq -r '.entries[]
    | select(.record.tool_name == "wallet.transfer" and .decision.type == "allowed")
    | .record.response_canonical | select(. != "") | fromjson | .tx_hash // empty' \
    "$(trace_file "$trace_id")")
[ -n "$tx_hashes" ] || fail "the program made no transfer"

say "The transaction on the chain"
for tx_hash in $tx_hashes; do
    cast tx "$tx_hash" --rpc-url "$RPC"
done

say "The signed trace"
jq . "$(trace_file "$trace_id")"

say "Each call's policy decision and provenance tag"
jq -r '
    def tag:
        if .type == "onchain" then "onchain(\(.chain), \(.block), \(.reference))"
        elif .type == "signed" then "signed(\(.by), \(.sig))"
        elif .type == "notarized" then "notarized(\(.scheme), \(.reference))"
        else .type end;
    .entries[] | "\(.record.seq)  \(.record.tool_name)  \(.decision.type)  \(.provenance | tag)"' \
    "$(trace_file "$trace_id")"
echo
echo "A tag is the tool server's own claim of where its answer came from, sealed into the signed trace; the gateway has not checked it."

jq -e '[.entries[] | select(.record.tool_name | startswith("wallet."))]
    | length > 0 and all(.provenance.type == "onchain")' \
    "$(trace_file "$trace_id")" > /dev/null || fail "a wallet call is not tagged onchain"
jq -e '[.entries[] | select(.record.tool_name | startswith("market."))]
    | length > 0 and all(.provenance.type == "unsigned")' \
    "$(trace_file "$trace_id")" > /dev/null || fail "a market call is not tagged unsigned"

# ── step 2 ────────────────────────────────────────────────────────────────────

banner "Step 2: the same run replays with the world switched off"

stop_world
say "anvil, demo-market and demo-wallet are stopped"
pgrep -fl 'anvil|demo-' || echo "(no chain, no tool servers)"

say "Replaying $trace_id with no network access"
"$GATEWAY" replay --config gateway.toml "$trace_id"

# ── step 3 ────────────────────────────────────────────────────────────────────

banner "Step 3: a tighter policy refuses the transfer at the call"

mv -f policy.toml.orig policy.toml
cp policy.toml policy.toml.orig
tightened=$(mktemp)
sed 's/amount_max = 50/amount_max = 10/' policy.toml > "$tightened"
mv "$tightened" policy.toml

say "policy.toml now says"
cat policy.toml

start_world

if [ -n "$AGENT_KEY" ]; then
    say "Running rebalance.lua, a program an agent wrote earlier, so this step is the same with or without a model"
else
    say "Running the same program again"
fi
denied=$("$CLIENT" execute rebalance.lua --request "$REQUEST")
echo "$denied"

denied_trace=$(printf '%s' "$denied" | jq -r .trace_id)
say "The refusal, in the trace"
jq '.entries[] | select(.decision.type == "denied_by_policy")
    | { tool: .record.tool_name, args: .record.args_canonical, decision: .decision }' \
    "$(trace_file "$denied_trace")"

# ── step 4 ────────────────────────────────────────────────────────────────────

banner "Step 4: a tool off the allow-list is absent from the API and refused"

cat > policy.toml << 'POLICY'
[principals.demo-agent]
allow = ["market.get_price", "wallet.get_balance"]
POLICY

say "policy.toml now says"
cat policy.toml

stop_gateway
start_gateway

say "The generated tool API no longer offers wallet.transfer"
"$CLIENT" description | sed -n '/# Tool API/,/# Examples/p'
if "$CLIENT" description | grep -q 'wallet.transfer{'; then
    fail "wallet.transfer is still in the generated API"
fi

say "The program that calls it through the tool.call primitive anyway (direct_transfer.lua)"
cat direct_transfer.lua

say "Executing it"
refused=$("$CLIENT" execute direct_transfer.lua)
echo "$refused"

refused_trace=$(printf '%s' "$refused" | jq -r .trace_id)
say "The refusal, in the trace"
jq '.entries[] | select(.decision.type == "denied_by_policy")
    | { tool: .record.tool_name, decision: .decision }' \
    "$(trace_file "$refused_trace")"

banner "Demo complete. Traces are in demo/traces/traces/."
