# Testing & Quality

## Makefile targets

```bash
make check             # CI gate: lint then test (must pass before merging)
make lint              # cargo fmt --check + cargo clippy --all-targets -D warnings
make test              # all tests: unit + integration
make fix               # auto-format + apply safe clippy fixes
make build             # cargo build
```

`make check` is the hard pre-commit gate. Run it before every commit. If it fails, fix before continuing.

Unlike proveno-core, `make lint` here runs clippy with `--all-targets`, so lints inside `tests/*.rs` and `examples/` are gated too.

`make test-prove` lives in proveno-zk, not here. The gateway does not prove, but a change to proveno-core that the gateway needs (the oracle tape, canonical serialization, the program hash) must pass it there before the core tag is bumped here.

`demo/` is a separate Cargo project and is not covered by `make check`. Build and run it on its own when you change it.

## Test mandate

Every feature commit must include at least one test for the new behaviour. Every bug fix must include a regression test that would have caught the bug. These are not optional: a commit that adds behaviour without a test, or fixes a bug without a regression test, is incomplete.

The gateway's guarantees are only as good as three properties, and any change that could affect one **must** include a test pinning it:

- **Replay is exact.** The same program and the same recorded responses give the same output, `gas_used` and `memory_used`, including for runs that failed.
- **The tool description is pure.** The same allowed schemas give the same text and `description_hash`, regardless of input order.
- **Credentials stay out.** No credential appears in a program, a tool description, a trace or anything in the store.

If a behaviour genuinely cannot be exercised without infrastructure that is unavailable in tests, document why in the PR description. This should be rare.

## Two test patterns

**Unit tests**: live in `#[cfg(test)]` modules inside the source file, importing from `super::*`. Use for anything that needs no network or VM run: policy decisions, schema checks, JSON to Lua value mapping, description rendering, trace signing, config parsing.

```rust
// src/policy.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_over_max_is_denied() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), EXAMPLE).unwrap();
        let policy = Policy::load(file.path()).unwrap();
        let args = serde_json::json!({ "to": "0x1", "amount": 60 });
        assert!(matches!(
            policy.check("demo-agent", "wallet.transfer", &args, &SessionState {}),
            Decision::Deny { .. }
        ));
    }
}
```

**Integration tests**: live in `tests/*.rs` and drive the public API with real wiring: a real rmcp connection to a mock downstream, a real VM run, a real store in a temp directory. `tests/common/mod.rs` provides the in-process mock downstream (`get_price`, `get_balance`, `transfer`); extend it by adding functions, never by changing existing ones, because other test files depend on it.

```rust
// tests/replay.rs
#[tokio::test]
async fn recorded_run_replays_with_downstream_stopped() {
    let downstream = common::start_mock_downstream().await;
    let (config, _dir) = common::gateway_config(&downstream.url());
    let engine = Engine::new(config.clone()).await.unwrap();
    let resp = engine.execute("demo-agent", request(PROGRAM)).await.unwrap();
    drop(engine);
    downstream.shutdown().await;

    let report = replay(&config, &resp.trace_id).unwrap();
    assert!(report.matched, "{:?}", report.mismatches);
}
```

## Clippy

`make lint` runs `cargo clippy --all-targets -- -D warnings`. Fix warnings rather than silencing them. If a targeted `#[allow(...)]` is genuinely required, attach a one-line comment explaining why. Never silence clippy globally at the crate or module level.

## What to test at each layer

| Layer | Pattern | Example |
|---|---|---|
| Config parsing and secret resolution | Unit test in `src/config.rs` | spec example parses; non-`env:` secret rejected |
| Policy, schema check, value mapping | Unit tests next to the code | amount over max denied; float becomes decimal string |
| Tool description and prelude | Unit + integration | same schemas give same hash; example programs compile |
| Lint | Integration through `compile_program` | `os.time()` on line 4 reports `line 4` |
| Trace signing and store | Unit tests | tampered entry fails verify; duplicate trace rejected |
| Downstream MCP client | Integration against `tests/common` | discovery order; bearer credential required |
| Host layer and execute engine | Integration against `tests/common` | denial recorded and catchable; credential absent from store |
| Agent-facing MCP server | Integration with an rmcp client | description lists only allowed tools; bad token gets 401 |
| Replay and determinism | Integration with the downstream shut down | success, caught denial, uncaught failure, gas exhaustion, divergence |
