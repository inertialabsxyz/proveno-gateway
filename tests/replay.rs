//! The determinism suite: runs recorded through a real engine against the mock
//! downstream, then replayed from the store with the downstream shut down.

mod common;

use std::sync::LazyLock;

use proveno::compiler::program_hash::compute_program_hash_sha256;
use proveno_gateway::config::{self, GatewayConfig};
use proveno_gateway::dialect::compile_program;
use proveno_gateway::engine::{Engine, ExecuteRequest};
use proveno_gateway::replay::{ReplayReport, replay};
use proveno_gateway::store::TraceStore;
use proveno_gateway::trace::{RunStatus, Trace, signing_key_from_hex};

const WALLET_CREDENTIAL_VAR: &str = "PROVENO_GATEWAY_REPLAY_TEST_WALLET_KEY";
const SIGNING_KEY_VAR: &str = "PROVENO_GATEWAY_REPLAY_TEST_SIGNING_KEY";
const TOKEN_VAR: &str = "PROVENO_GATEWAY_REPLAY_TEST_TOKEN";
const SIGNING_KEY_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";

/// Sets the secret variables once, before any test reads the environment.
static ENV: LazyLock<()> = LazyLock::new(|| {
    // SAFETY: the LazyLock runs this exactly once, and every test in this
    // binary forces it before touching the environment or spawning anything.
    unsafe {
        std::env::set_var(WALLET_CREDENTIAL_VAR, common::MOCK_BEARER);
        std::env::set_var(SIGNING_KEY_VAR, SIGNING_KEY_HEX);
        std::env::set_var(TOKEN_VAR, "demo-agent-token");
    }
});

const POLICY: &str = r#"
[principals.demo-agent]
allow = ["wallet.get_balance", "market.get_price", "wallet.transfer"]

[constraints."wallet.transfer"]
amount_max = 50
"#;

/// A gateway config over the mock downstream, loaded from a file. `vm` is extra
/// `[vm]` TOML.
fn gateway_config(url: &str, vm: &str) -> (GatewayConfig, tempfile::TempDir) {
    LazyLock::force(&ENV);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    let text = format!(
        r#"
[server]
listen = "127.0.0.1:0"
signing_key = "env:{SIGNING_KEY_VAR}"

[vm]
max_tool_calls = 64
{vm}

[[downstream]]
name = "wallet"
transport = "http"
url = "{url}"
credential = "env:{WALLET_CREDENTIAL_VAR}"

[[downstream]]
name = "market"
transport = "http"
url = "{url}"

[policy]
file = "policy.toml"

[store]
dir = "store"

[principals.demo-agent]
token = "env:{TOKEN_VAR}"
"#
    );
    let path = dir.path().join("gateway.toml");
    std::fs::write(&path, text).unwrap();
    (config::load(&path).unwrap(), dir)
}

/// Records each program in order through one engine, then drops the engine and
/// shuts the downstream down, so everything after runs with no network.
async fn record(vm: &str, programs: &[&str]) -> (GatewayConfig, tempfile::TempDir, Vec<String>) {
    let mock = common::start_mock_downstream().await;
    let (config, dir) = gateway_config(&mock.url(), vm);
    let engine = Engine::new(config.clone()).await.unwrap();
    let mut trace_ids = Vec::new();
    for program in programs {
        let resp = engine
            .execute(
                "demo-agent",
                ExecuteRequest {
                    program: program.to_string(),
                    session: None,
                    request: None,
                },
            )
            .await
            .unwrap();
        trace_ids.push(resp.trace_id);
    }
    drop(engine);
    mock.shutdown().await;
    (config, dir, trace_ids)
}

fn store(config: &GatewayConfig) -> TraceStore {
    TraceStore::open(&config.store.dir).unwrap()
}

fn trace_path(config: &GatewayConfig, trace_id: &str) -> std::path::PathBuf {
    config.store.dir.join(format!("traces/{trace_id}.json"))
}

/// Overwrites a stored trace in place, bypassing the append-only store.
fn overwrite_trace(config: &GatewayConfig, trace: &Trace) {
    std::fs::write(
        trace_path(config, &trace.header.trace_id),
        serde_json::to_vec(trace).unwrap(),
    )
    .unwrap();
}

/// Replays and asserts a match with the footer's output, status kind and meters.
fn assert_replays(config: &GatewayConfig, trace_id: &str) -> (Trace, ReplayReport) {
    let report = replay(config, trace_id).unwrap();
    assert!(report.matched, "{:?}", report.mismatches);
    assert!(report.mismatches.is_empty());
    let trace = store(config).get_trace(trace_id).unwrap();
    assert_eq!(report.output, trace.footer.output);
    assert_eq!(report.gas_used, trace.footer.gas_used);
    assert_eq!(report.memory_used, trace.footer.memory_used);
    (trace, report)
}

fn error_kind(status: &RunStatus) -> &str {
    match status {
        RunStatus::Error { kind, .. } => kind,
        RunStatus::Ok => panic!("expected an error status"),
    }
}

const SUCCESS: &str = r#"
local p = market.get_price{ pair = "ETH/USD" }
local b = wallet.get_balance{ address = "0x1" }
local t = wallet.transfer{ to = "0x1", amount = 20 }
return { price = p.price, usdc = b.usdc, tx = t.tx }
"#;

#[tokio::test(flavor = "multi_thread")]
async fn successful_run_replays_and_matches() {
    let (config, _dir, ids) = record("", &[SUCCESS]).await;
    let (trace, report) = assert_replays(&config, &ids[0]);
    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(trace.entries.len(), 3);
    assert_eq!(
        report.output.as_deref(),
        Some(r#"{"price":"2500.5","tx":"0xabc","usdc":5000}"#)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn caught_denial_replays_and_matches() {
    let (config, _dir, ids) = record(
        "",
        &[r#"
local ok, err = pcall(function()
  return wallet.transfer{ to = "0x1", amount = 60 }
end)
return { ok = ok, error = err }
"#],
    )
    .await;
    let (trace, report) = assert_replays(&config, &ids[0]);
    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(trace.footer.status, RunStatus::Ok);
    assert!(
        report.output.as_deref().unwrap().contains("amount_max 50"),
        "{:?}",
        report.output
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn uncaught_denial_replays_to_the_same_failure() {
    let (config, _dir, ids) = record(
        "",
        &[r#"
local b = wallet.get_balance{ address = "0x1" }
wallet.transfer{ to = "0x1", amount = 60 }
return b.usdc
"#],
    )
    .await;
    let (trace, report) = assert_replays(&config, &ids[0]);
    assert_eq!(error_kind(&trace.footer.status), "ToolError");
    assert_eq!(error_kind(&report.status), "ToolError");
    assert_eq!(report.output, None);
    assert_eq!(trace.entries.len(), 2);
    assert!(report.gas_used > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn gas_exhausted_run_replays_to_the_same_failure() {
    let (config, _dir, ids) = record(
        "gas_limit = 5000",
        &[r#"
local b = wallet.get_balance{ address = "0x1" }
local n = 0
while true do n = n + b.usdc end
return n
"#],
    )
    .await;
    let (trace, report) = assert_replays(&config, &ids[0]);
    assert_eq!(trace.header.vm_config.gas_limit, 5000);
    assert_eq!(error_kind(&trace.footer.status), "GasExhausted");
    assert_eq!(error_kind(&report.status), "GasExhausted");
    assert_eq!(trace.entries.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn output_limit_is_applied_on_replay() {
    let (config, _dir, ids) = record(
        "max_output_bytes = 8",
        &[r#"local b = wallet.get_balance{ address = "0x1" } return b"#],
    )
    .await;
    let (trace, report) = assert_replays(&config, &ids[0]);
    assert_eq!(error_kind(&trace.footer.status), "OutputExceeded");
    assert_eq!(error_kind(&report.status), "OutputExceeded");
}

#[tokio::test(flavor = "multi_thread")]
async fn sources_differing_in_a_comment_both_replay() {
    // `tool.call` directly, so the error is raised on a program line rather
    // than inside the prelude wrapper, whose line the engine drops.
    let body = r#"local b = wallet.get_balance{ address = "0x1" }
tool.call("wallet.transfer", { to = "0x1", amount = 60 })
return b.usdc
"#;
    let commented = format!("-- moves funds, which policy denies\n{body}");
    let (config, _dir, ids) = record("", &[body, &commented]).await;

    let store = store(&config);
    let first = store.get_trace(&ids[0]).unwrap();
    let second = store.get_trace(&ids[1]).unwrap();
    assert_eq!(first.header.program_hash, second.header.program_hash);
    let (RunStatus::Error { message: m1, .. }, RunStatus::Error { message: m2, .. }) =
        (&first.footer.status, &second.footer.status)
    else {
        panic!("both runs should fail");
    };
    assert!(m1.starts_with("line 2:"), "{m1}");
    assert!(m2.starts_with("line 3:"), "{m2}");

    for id in &ids {
        let (_, report) = assert_replays(&config, id);
        assert_eq!(error_kind(&report.status), "ToolError");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_argument_is_reported_as_a_divergence() {
    let (config, _dir, ids) = record("", &[SUCCESS]).await;
    let store = store(&config);
    let mut trace = store.get_trace(&ids[0]).unwrap();

    let source = store.get_program(&trace.header.program_hash).unwrap();
    let edited = source.replace(r#"address = "0x1""#, r#"address = "0x2""#);
    assert_ne!(edited, source);
    let hash = hex::encode(compute_program_hash_sha256(
        &compile_program("", &edited).unwrap().prototypes,
    ));
    assert_ne!(hash, trace.header.program_hash);
    store.put_program(&hash, &edited).unwrap();
    trace.header.program_hash = hash;
    trace.sign(&signing_key_from_hex(SIGNING_KEY_HEX).unwrap());
    overwrite_trace(&config, &trace);

    let report = replay(&config, &ids[0]).unwrap();
    assert!(!report.matched);
    assert_eq!(
        report.mismatches[0],
        r#"divergence at seq 1: expected wallet.get_balance {"address":"0x1"}, actual wallet.get_balance {"address":"0x2"}"#
    );
    assert!(
        report
            .mismatches
            .iter()
            .any(|m| m == "tape: 2 of 3 recorded calls left unconsumed"),
        "{:?}",
        report.mismatches
    );
    assert!(
        report
            .mismatches
            .iter()
            .any(|m| m == "status: recorded ok, replayed ToolError"),
        "{:?}",
        report.mismatches
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stored_source_that_misses_its_hash_is_reported() {
    let (config, _dir, ids) = record("", &[SUCCESS]).await;
    let trace = store(&config).get_trace(&ids[0]).unwrap();
    let path = config
        .store
        .dir
        .join(format!("programs/{}.lua", trace.header.program_hash));
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, source.replace("amount = 20", "amount = 21")).unwrap();

    let report = replay(&config, &ids[0]).unwrap();
    assert!(!report.matched);
    assert!(
        report.mismatches[0].starts_with(&format!(
            "program_hash: header has {}",
            trace.header.program_hash
        )),
        "{:?}",
        report.mismatches
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_trace_fails_signature_verification() {
    let (config, _dir, ids) = record("", &[SUCCESS]).await;
    let mut trace = store(&config).get_trace(&ids[0]).unwrap();
    let response = &mut trace.entries[1].record.response_canonical;
    let at = response.iter().position(|&b| b == b'5').unwrap();
    response[at] = b'6';
    overwrite_trace(&config, &trace);

    let err = replay(&config, &ids[0]).unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("signature verification failed"),
        "{message}"
    );
}
