# proveno-gateway

An MCP gateway for [proveno](https://github.com/inertialabsxyz/proveno). Agents
submit Lua programs through one tool, `execute`. Every tool call a program makes
is policy-checked, has its credential injected by the gateway, is dispatched to
a downstream MCP server, and is recorded in a signed trace that replays
bit-for-bit with the network off.

The gateway makes no model calls: it receives programs, not tasks.

Depends on [proveno-core](https://github.com/inertialabsxyz/proveno-core) by git
tag.

```bash
make check
```

## How a run works

The agent's only tool is `execute`. Its description is the generated prompt: the
dialect rules, the typed API for the tools this principal may call, and example
programs. The program the model writes is compiled together with a generated
prelude, so `program_hash` covers both.

```mermaid
sequenceDiagram
    participant A as Agent
    participant S as MCP server
    participant E as Engine
    participant V as Lua VM
    participant H as Host layer
    participant D as Downstream MCP server
    participant T as Trace store

    A->>S: execute(program, session?, request?)
    Note over S: Bearer token maps to a principal, or 401
    S->>E: execute(principal, request)
    E->>E: build description and prelude for this principal
    E->>E: lint and compile prelude + program
    Note over E: A lint error returns line N: message<br/>and nothing is stored
    E->>T: put program by program_hash, description by description_hash
    E->>V: run with the configured VmConfig

    loop every tool.call
        V->>H: call_tool(name, args)
        H->>H: 1. args to JSON
        H->>H: 2. allowed for this principal?
        H->>H: 3. schema check
        H->>H: 4. policy check
        Note over H: A refusal at 2, 3 or 4 is recorded<br/>and raised as a catchable Lua error
        H->>D: 5. tools/call, credential attached
        D-->>H: response
        H->>H: 6. map to integers, no floats
        H-->>V: table
    end

    V-->>E: return value, gas, memory, transcript
    E->>E: zip transcript with the host's decisions
    E->>T: sign and store the trace
    E-->>S: result, trace_id, status
    S-->>A: structured result
```

The credential is attached inside the gateway's MCP client, at the connection,
so the program and the model never see it. A run that fails still produces a
trace.

## Trace and replay

The trace wraps proveno-core's transcript: each entry is the record core made,
plus what the gateway knows and core does not.

```mermaid
flowchart LR
    subgraph Trace
        direction TB
        HDR["header<br/>trace_id, session, principal<br/>program_hash, policy_hash, description_hash<br/>vm_version, vm_config, request"]
        ENT["entries<br/>record + decision + provenance,<br/>one per tool call"]
        FTR["footer<br/>output, status, gas_used,<br/>memory_used, signature"]
        HDR --- ENT --- FTR
    end

    subgraph Store["Trace store"]
        direction TB
        P["programs/&lt;program_hash&gt;.lua<br/>prelude + program"]
        DSC["descriptions/&lt;description_hash&gt;.txt<br/>what the model was told"]
    end

    Trace -. program_hash .-> P
    Trace -. description_hash .-> DSC
```

`replay` rebuilds the run from that record alone, with no downstream connection.

```mermaid
flowchart TD
    L["load trace by trace_id"] --> V{"signature verifies?"}
    V -- no --> F["fail, do not replay"]
    V -- yes --> C["load program by program_hash,<br/>compile, recheck the hash"]
    C --> TAPE["OracleTape from the entries' records"]
    TAPE --> RUN["Vm with the header's vm_config<br/>and a strict TapeHost"]
    RUN --> CMP{"compare"}
    CMP --> D1["divergence: a call whose name or<br/>canonical args differ from the record"]
    CMP --> D2["recorded calls left unconsumed"]
    CMP --> D3["different output"]
    CMP --> D4["different status kind"]
    CMP --> D5["different gas_used or memory_used"]
    D1 & D2 & D3 & D4 & D5 --> R["matched only if there are no mismatches"]
```

Only the status kind is compared, not its message: `program_hash` deliberately
excludes line numbers, so a message's line can differ between two sources that
compile to the same bytecode.

## Demo

`demo/` runs the whole thing against a local Anvil chain: an agent rebalances a
wallet, the run replays with everything stopped, and a policy change is enforced
at the call. See [demo/README.md](demo/README.md).

## Status

Complete through milestone 4 of the spec: MCP edges, host layer, replay and the
demo. Provenance is bind-only and always `unsigned` in this prototype, and no
proof is generated. The spec is `planning/proveno-gateway-spec.md` in the
umbrella repository.

## Licence

See [LICENSE](LICENSE).
