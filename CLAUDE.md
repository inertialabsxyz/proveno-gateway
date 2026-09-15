# CLAUDE.md

Guidance for Claude Code working in **proveno-gateway**.

## What this repository is

An application of the proveno runtime: an MCP server whose one tool, `execute`,
runs an agent-written Lua program. Every tool call the program makes is
schema- and policy-checked, has its credential injected here, is dispatched to a
downstream MCP server, and is recorded in a signed trace that replays
bit-for-bit.

It never calls a model. It receives programs, not tasks; the LLM loop is
proveno-agent's.

| Repository | Scope |
|---|---|
| [proveno-core](https://github.com/inertialabsxyz/proveno-core) | Runtime: parser, compiler, bytecode, vm, host, isa, record/replay |
| [proveno-zk](https://github.com/inertialabsxyz/proveno-zk) | Policy, commitments, Noir circuit, OpenVM guest, contracts |
| [proveno-agent](https://github.com/inertialabsxyz/proveno-agent) | LLM orchestrator, demo server, TLS provenance |
| **proveno-gateway** (here) | MCP edges, host policy, credential injection, signed trace, replay |
| [proveno](https://github.com/inertialabsxyz/proveno) | Umbrella: project overview, architecture, trust model |

Depends on proveno-core only, pinned by git tag. The spec is
`planning/proveno-gateway-spec.md` in the umbrella. The
[architecture document](https://github.com/inertialabsxyz/proveno/blob/main/docs/architecture.md)
is the tie-breaker when documents disagree.

## Quality Gate

```bash
make check      # fmt + clippy --all-targets -D warnings + tests
```

Must pass before every commit.

## Decisions that are not up for re-litigation

- **`tool.call` is the only primitive.** Per-tool functions such as
  `wallet.transfer{...}` are a generated `local` prelude over it. Core rejects
  unknown globals, so the prelude is compiled together with the program.
- **`program_hash` is `compute_program_hash_sha256`.** The Poseidon2 hash does
  not cover constants. proveno-core is used with `poseidon` off.
- **Replay builds on core's `OracleTape` / `TapeHost`.** Anything missing, such
  as matching tool name and args for divergence reports, is added to
  proveno-core, not reimplemented here.
- **`VmConfig` goes in the signed trace header.** Replay runs with it.
- **Provenance is bind-only.** The trace carries attestation blobs and a tag
  naming the provider; nothing here verifies them.
- **No floats in the VM.** Non-integer numbers from downstream servers are
  mapped at the host boundary, never passed through.

## Commits

Scopes: `mcp`, `host`, `policy`, `trace`, `replay`, `config`, `tests`, `docs`,
`repo`.
