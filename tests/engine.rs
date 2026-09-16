//! The execute engine end to end: a real downstream over rmcp, a real VM run,
//! and a real store in a temporary directory.

mod common;

use std::path::Path;
use std::sync::LazyLock;
use std::sync::atomic::Ordering;

use proveno::ToolCallStatus;
use proveno::compiler::program_hash::compute_program_hash_sha256;
use proveno_gateway::config::{self, GatewayConfig};
use proveno_gateway::dialect::compile_program;
use proveno_gateway::engine::{Engine, ExecuteError, ExecuteRequest};
use proveno_gateway::store::TraceStore;
use proveno_gateway::trace::{CallDecision, Provenance, RunStatus, Trace, signing_key_from_hex};
use rmcp::model::Tool;
use serde_json::json;

const WALLET_CREDENTIAL_VAR: &str = "PROVENO_GATEWAY_ENGINE_TEST_WALLET_KEY";
const SIGNING_KEY_VAR: &str = "PROVENO_GATEWAY_ENGINE_TEST_SIGNING_KEY";
const TOKEN_VAR: &str = "PROVENO_GATEWAY_ENGINE_TEST_TOKEN";
const SIGNING_KEY_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TOKEN: &str = "demo-agent-token";

/// Sets the secret variables once, before any test reads the environment.
/// Every test calls this first, so no test thread reads the environment while
/// it is being written.
static ENV: LazyLock<()> = LazyLock::new(|| {
    // SAFETY: the LazyLock runs this exactly once, and every test in this
    // binary forces it before touching the environment or spawning anything.
    unsafe {
        std::env::set_var(WALLET_CREDENTIAL_VAR, common::MOCK_BEARER);
        std::env::set_var(SIGNING_KEY_VAR, SIGNING_KEY_HEX);
        std::env::set_var(TOKEN_VAR, TOKEN);
    }
});

const POLICY: &str = r#"
[principals.demo-agent]
allow = ["wallet.get_balance", "market.get_price", "wallet.transfer"]

[constraints."wallet.transfer"]
amount_max = 50
"#;

/// A gateway config over the mock downstream, loaded from a file so relative
/// paths resolve as in production. `vm` is extra `[vm]` TOML.
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

fn request(program: &str) -> ExecuteRequest {
    ExecuteRequest {
        program: program.to_string(),
        session: Some("session-1".into()),
        request: Some("rebalance if the price moved".into()),
    }
}

fn stored_trace(config: &GatewayConfig, trace_id: &str) -> Trace {
    TraceStore::open(&config.store.dir)
        .unwrap()
        .get_trace(trace_id)
        .unwrap()
}

fn verify(trace: &Trace) {
    let key = signing_key_from_hex(SIGNING_KEY_HEX).unwrap();
    trace.verify(&key.verifying_key()).unwrap();
}

fn decisions(trace: &Trace) -> Vec<CallDecision> {
    trace.entries.iter().map(|e| e.decision.clone()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn section_4_flow_produces_a_signed_trace() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute(
            "demo-agent",
            request(
                r#"
local p = market.get_price{ pair = "ETH/USD" }
local t = wallet.transfer{ to = "0x1", amount = 20 }
return { price = p.price, price_type = type(p.price), tx = t.tx }
"#,
            ),
        )
        .await
        .unwrap();

    assert_eq!(resp.status, RunStatus::Ok);
    // 2500.5 is not an integer, so it reached the program as a decimal string.
    assert_eq!(
        resp.result,
        Some(json!({ "price": "2500.5", "price_type": "string", "tx": "0xabc" }))
    );

    let trace = stored_trace(&config, &resp.trace_id);
    verify(&trace);
    assert_eq!(
        decisions(&trace),
        [CallDecision::Allowed, CallDecision::Allowed]
    );
    assert_eq!(trace.entries[0].record.tool_name, "market.get_price");
    assert_eq!(trace.entries[1].record.tool_name, "wallet.transfer");
    assert_eq!(trace.header.principal, "demo-agent");
    assert_eq!(trace.header.session.as_deref(), Some("session-1"));
    assert_eq!(
        trace.header.request.as_deref(),
        Some("rebalance if the price moved")
    );
    assert_eq!(trace.header.vm_version, proveno_gateway::VM_VERSION);
    assert_eq!(trace.header.vm_config, config.vm);
    assert_eq!(
        trace.footer.output.as_deref(),
        Some(r#"{"price":"2500.5","price_type":"string","tx":"0xabc"}"#)
    );
    assert!(trace.footer.gas_used > 0);

    let store = TraceStore::open(&config.store.dir).unwrap();
    let description = engine.description_for("demo-agent");
    assert_eq!(trace.header.description_hash, hex::encode(description.hash));
    assert_eq!(
        store
            .get_description(&trace.header.description_hash)
            .unwrap(),
        description.text
    );
    // The stored program is the whole compiled source, so it recompiles to the
    // recorded hash on its own.
    let source = store.get_program(&trace.header.program_hash).unwrap();
    let recompiled = compile_program("", &source).unwrap();
    assert_eq!(
        hex::encode(compute_program_hash_sha256(&recompiled.prototypes)),
        trace.header.program_hash
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn caught_denial_is_recorded_and_the_run_succeeds() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute(
            "demo-agent",
            request(
                r#"
local ok, err = pcall(function()
  return wallet.transfer{ to = "0x1", amount = 60 }
end)
return { ok = ok, error = err }
"#,
            ),
        )
        .await
        .unwrap();

    assert_eq!(resp.status, RunStatus::Ok);
    assert_eq!(
        resp.result,
        Some(json!({
            "ok": false,
            "error": "policy: wallet.transfer: amount 60 exceeds amount_max 50"
        }))
    );
    let trace = stored_trace(&config, &resp.trace_id);
    verify(&trace);
    assert_eq!(
        decisions(&trace),
        [CallDecision::DeniedByPolicy {
            reason: "wallet.transfer: amount 60 exceeds amount_max 50".into()
        }]
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disallowed_tool_is_absent_and_denied() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let text = engine.description_for("demo-agent").text;
    assert!(text.contains("market.get_price{"), "{text}");
    assert!(!text.contains("wallet.get_price"), "{text}");
    assert!(!text.contains("market.transfer"), "{text}");

    let resp = engine
        .execute(
            "demo-agent",
            request(
                r#"
local ok, err = pcall(function()
  return tool.call("wallet.get_price", { pair = "ETH/USD" })
end)
return err
"#,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.result,
        Some(json!(
            "policy: tool wallet.get_price is not allowed for demo-agent"
        ))
    );
    let trace = stored_trace(&config, &resp.trace_id);
    assert!(matches!(
        decisions(&trace)[..],
        [CallDecision::DeniedByPolicy { .. }]
    ));
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_failure_is_recorded() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute(
            "demo-agent",
            request(
                r#"
local ok, err = pcall(function()
  return wallet.transfer{ to = "0x1", amount = "twenty" }
end)
return ok
"#,
            ),
        )
        .await
        .unwrap();
    assert_eq!(resp.result, Some(json!(false)));
    let trace = stored_trace(&config, &resp.trace_id);
    assert!(matches!(
        decisions(&trace)[..],
        [CallDecision::RejectedBySchema { .. }]
    ));
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lint_failure_reports_the_agent_line_and_stores_nothing() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let program = "local a = 1\nlocal b = 2\nlocal c = 3\nlocal t = os.time()\nreturn t\n";
    let Err(ExecuteError::Lint(err)) = engine.execute("demo-agent", request(program)).await else {
        panic!("expected a lint error");
    };
    assert_eq!(err.line, 4);
    assert!(err.message.contains("`os` is not available"), "{err}");
    // The model sees the lint error exactly as `check` reports it.
    assert_eq!(
        ExecuteError::Lint(err.clone()).to_string(),
        format!("line 4: {}", err.message)
    );
    assert_eq!(engine.check("demo-agent", program), Err(err));
    assert!(engine.check("demo-agent", "return 1").is_ok());

    for kind in ["traces", "programs", "descriptions"] {
        let dir = config.store.dir.join(kind);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "{kind}");
    }
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uncaught_denial_fails_the_run_and_still_records_it() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute(
            "demo-agent",
            request(
                r#"
local b = wallet.get_balance{ address = "0x1" }
wallet.transfer{ to = "0x1", amount = 60 }
return b.usdc
"#,
            ),
        )
        .await
        .unwrap();

    let RunStatus::Error { kind, message } = &resp.status else {
        panic!("expected an error, got {:?}", resp.status);
    };
    assert_eq!(kind, "ToolError");
    assert!(
        message.contains("policy: wallet.transfer: amount 60 exceeds amount_max 50"),
        "{message}"
    );
    assert_eq!(resp.result, None);

    let trace = stored_trace(&config, &resp.trace_id);
    verify(&trace);
    assert_eq!(trace.footer.status, resp.status);
    assert_eq!(trace.footer.output, None);
    assert!(trace.footer.gas_used > 0);
    assert!(matches!(
        decisions(&trace)[..],
        [CallDecision::Allowed, CallDecision::DeniedByPolicy { .. }]
    ));
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exhausted_limits_fail_the_run_with_a_trace() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "gas_limit = 5000\nmax_output_bytes = 8");
    let engine = Engine::new(config.clone()).await.unwrap();

    let gas = engine
        .execute(
            "demo-agent",
            request("local n = 0\nwhile true do n = n + 1 end\nreturn n"),
        )
        .await
        .unwrap();
    assert!(
        matches!(&gas.status, RunStatus::Error { kind, .. } if kind == "GasExhausted"),
        "{:?}",
        gas.status
    );
    let trace = stored_trace(&config, &gas.trace_id);
    verify(&trace);
    assert_eq!(trace.header.vm_config.gas_limit, 5000);

    let output = engine
        .execute("demo-agent", request(r#"return "more than eight bytes""#))
        .await
        .unwrap();
    assert_eq!(
        output.status,
        RunStatus::Error {
            kind: "OutputExceeded".into(),
            message: "output exceeded".into()
        }
    );
    assert_eq!(stored_trace(&config, &output.trace_id).footer.output, None);
    mock.shutdown().await;
}

fn files_containing(dir: &Path, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            hits.extend(files_containing(&path, needle));
        } else if std::fs::read_to_string(&path).unwrap().contains(needle) {
            hits.push(path.display().to_string());
        }
    }
    hits
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_the_store() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    // The transfer succeeds only because the credential was attached.
    let resp = engine
        .execute(
            "demo-agent",
            request(r#"return wallet.transfer{ to = "0x1", amount = 20 }"#),
        )
        .await
        .unwrap();
    assert_eq!(resp.status, RunStatus::Ok);
    engine
        .execute(
            "demo-agent",
            request(
                r#"local ok, err = pcall(function() return wallet.transfer{ to = "0x1", amount = 99 } end) return err"#,
            ),
        )
        .await
        .unwrap();

    for secret in [common::MOCK_BEARER, SIGNING_KEY_HEX, TOKEN] {
        assert_eq!(
            files_containing(&config.store.dir, secret),
            Vec::<String>::new(),
            "{secret}"
        );
    }
    assert!(
        !engine
            .description_for("demo-agent")
            .text
            .contains(common::MOCK_BEARER)
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn same_config_gives_the_same_description_hash() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let first = Engine::new(config.clone()).await.unwrap();
    let second = Engine::new(config).await.unwrap();
    assert_eq!(
        first.description_for("demo-agent").hash,
        second.description_for("demo-agent").hash
    );
    assert_ne!(
        first.description_for("demo-agent").hash,
        first.description_for("nobody").hash
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_secret_fails_at_startup() {
    let mock = common::start_mock_downstream().await;
    let (mut config, dir) = gateway_config(&mock.url(), "");
    let text = "[server]\nlisten = \"x\"\nsigning_key = \"env:PROVENO_GATEWAY_ENGINE_TEST_UNSET\"\n\
                [policy]\nfile = \"p\"\n[store]\ndir = \"s\"\n";
    let path = dir.path().join("unset.toml");
    std::fs::write(&path, text).unwrap();
    config.server = config::load(&path).unwrap().server;

    let err = Engine::new(config).await.err().unwrap();
    let message = format!("{err:#}");
    assert!(message.contains("server.signing_key"), "{message}");
    assert!(
        message.contains("PROVENO_GATEWAY_ENGINE_TEST_UNSET"),
        "{message}"
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allowed_tool_lua_cannot_declare_fails_at_startup() {
    let mock = common::start_mock_downstream().await;
    let (mut config, _dir) = gateway_config(&mock.url(), "");
    // `string` is a valid config name, but as a prelude local it would shadow
    // the string library.
    config.downstream[1].name = "string".into();
    std::fs::write(
        &config.policy.file,
        "[principals.demo-agent]\nallow = [\"string.get_price\"]\n",
    )
    .unwrap();

    let err = Engine::new(config).await.err().unwrap().to_string();
    assert!(err.contains("principal `demo-agent`"), "{err}");
    assert!(
        err.contains("`string` is not usable as a Lua name"),
        "{err}"
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sources_with_the_same_bytecode_both_run() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let first = engine
        .execute("demo-agent", request("return 1"))
        .await
        .unwrap();
    let second = engine
        .execute("demo-agent", request("-- the same program\nreturn 1"))
        .await
        .unwrap();
    assert_eq!(second.status, RunStatus::Ok);
    assert_eq!(
        stored_trace(&config, &first.trace_id).header.program_hash,
        stored_trace(&config, &second.trace_id).header.program_hash
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unwritable_store_is_an_error_not_a_panic() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    // A file where the traces directory should be: the run happens, but its
    // trace cannot be written.
    let traces = config.store.dir.join("traces");
    std::fs::remove_dir(&traces).unwrap();
    std::fs::write(&traces, "not a directory").unwrap();

    let result = engine.execute("demo-agent", request("return 1")).await;
    let Err(ExecuteError::Store(e)) = result else {
        panic!("expected a store error, got {result:?}");
    };
    assert!(
        ExecuteError::Store(e)
            .to_string()
            .starts_with("trace store: ")
    );
    mock.shutdown().await;
}

const BALANCE_PROVENANCE: &str =
    r#"{"block":12,"chain":"31337","reference":"0xblockhash","type":"onchain"}"#;

/// A downstream serving the three tools the policy names: `get_balance` and
/// `transfer` report `onchain` provenance, `get_price` reports none.
async fn start_attesting() -> common::AttestingDownstream {
    let schema = common::open_object_schema;
    common::start_attesting_downstream(vec![
        common::AttestingTool {
            tool: Tool::new("get_balance", "Balance.", schema()),
            response: json!({ "eth_milli": 620 }),
            // Deliberately not in canonical key order.
            provenance: Some(json!({
                "type": "onchain", "reference": "0xblockhash", "chain": "31337", "block": 12
            })),
        },
        common::AttestingTool {
            tool: Tool::new("transfer", "Transfer.", schema()),
            response: json!({ "tx_hash": "0xtx" }),
            provenance: Some(json!({
                "type": "onchain", "chain": "31337", "block": 13, "reference": "0xtx"
            })),
        },
        common::AttestingTool {
            tool: Tool::new("get_price", "Price.", schema()),
            response: json!({ "price": 2500 }),
            provenance: None,
        },
    ])
    .await
}

/// An attested read, then an unattested one, then a transfer the policy denies.
const ATTESTED_PROGRAM: &str = r#"
local b = wallet.get_balance{ address = "0x1" }
local p = market.get_price{ pair = "ETH/USD" }
local ok, err = pcall(function()
  return wallet.transfer{ to = "0x1", amount = 60 }
end)
return { eth_milli = b.eth_milli, price = p.price, denied = err }
"#;

#[tokio::test(flavor = "multi_thread")]
async fn reported_provenance_is_tagged_in_the_entry_and_bound_in_the_record() {
    let attesting = start_attesting().await;
    let (config, _dir) = gateway_config(&attesting.downstream.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute("demo-agent", request(ATTESTED_PROGRAM))
        .await
        .unwrap();
    assert_eq!(resp.status, RunStatus::Ok);
    // Provenance travels beside the response, never in the value the program sees.
    assert_eq!(
        resp.result,
        Some(json!({
            "eth_milli": 620,
            "price": 2500,
            "denied": "policy: wallet.transfer: amount 60 exceeds amount_max 50",
        }))
    );

    let trace = stored_trace(&config, &resp.trace_id);
    verify(&trace);
    let balance = &trace.entries[0];
    assert_eq!(
        balance.provenance,
        Provenance::Onchain {
            chain: "31337".into(),
            block: 12,
            reference: "0xblockhash".into(),
        }
    );
    assert_eq!(balance.record.attestation, BALANCE_PROVENANCE.as_bytes());
    // The next call reported nothing, so it gets no attestation, not the last one.
    let price = &trace.entries[1];
    assert_eq!(price.provenance, Provenance::Unsigned);
    assert!(price.record.attestation.is_empty());
    for entry in &trace.entries {
        let response = String::from_utf8_lossy(&entry.record.response_canonical);
        assert!(!response.contains("proveno"), "{response}");
        assert!(!response.contains("0xblockhash"), "{response}");
    }
    attesting.downstream.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provenance_attestation_is_byte_identical_across_runs() {
    let attesting = start_attesting().await;
    let (config, _dir) = gateway_config(&attesting.downstream.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let mut blobs = Vec::new();
    for _ in 0..2 {
        let resp = engine
            .execute("demo-agent", request(ATTESTED_PROGRAM))
            .await
            .unwrap();
        let trace = stored_trace(&config, &resp.trace_id);
        blobs.push(
            trace
                .entries
                .iter()
                .map(|e| e.record.attestation.clone())
                .collect::<Vec<_>>(),
        );
    }
    assert!(!blobs[0][0].is_empty());
    assert_eq!(blobs[0], blobs[1]);
    attesting.downstream.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_call_gets_no_provenance_or_attestation() {
    let attesting = start_attesting().await;
    let (config, _dir) = gateway_config(&attesting.downstream.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute("demo-agent", request(ATTESTED_PROGRAM))
        .await
        .unwrap();
    let trace = stored_trace(&config, &resp.trace_id);
    let denied = &trace.entries[2];
    assert_eq!(denied.record.tool_name, "wallet.transfer");
    assert!(matches!(
        denied.decision,
        CallDecision::DeniedByPolicy { .. }
    ));
    assert_eq!(denied.provenance, Provenance::Unsigned);
    assert!(denied.record.attestation.is_empty());
    // The downstream would have attested the transfer, but it was never asked.
    assert_eq!(attesting.calls.load(Ordering::SeqCst), 2);
    attesting.downstream.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_provenance_fails_the_call_catchably() {
    let attesting = common::start_attesting_downstream(vec![common::AttestingTool {
        tool: Tool::new("get_balance", "Balance.", common::open_object_schema()),
        response: json!({ "eth_milli": 620 }),
        provenance: Some(json!({ "type": "onchain", "chain": "31337", "block": 12 })),
    }])
    .await;
    let (config, _dir) = gateway_config(&attesting.downstream.url(), "");
    let engine = Engine::new(config.clone()).await.unwrap();

    let resp = engine
        .execute(
            "demo-agent",
            request(
                r#"
local ok, err = pcall(function() return wallet.get_balance{ address = "0x1" } end)
return { ok = ok, error = err }
"#,
            ),
        )
        .await
        .unwrap();
    assert_eq!(resp.status, RunStatus::Ok);
    let result = resp.result.unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(
        result["error"],
        "downstream: wallet.get_balance: malformed provenance from downstream `wallet`: \
         `proveno/provenance`: missing field `reference`"
    );

    let trace = stored_trace(&config, &resp.trace_id);
    let entry = &trace.entries[0];
    assert_eq!(entry.decision, CallDecision::Allowed);
    assert_eq!(entry.record.status, ToolCallStatus::Error);
    assert_eq!(entry.provenance, Provenance::Unsigned);
    assert!(entry.record.attestation.is_empty());
    attesting.downstream.shutdown().await;
}
