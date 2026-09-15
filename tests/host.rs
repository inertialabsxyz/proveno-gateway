//! The host layer inside a real VM run: each refusing stage is recorded and
//! surfaces to the program as a catchable, stage-prefixed error, and the host
//! log lines up with core's transcript call for call.

mod common;

use std::sync::Arc;

use proveno::host::canonicalize::canonical_serialize;
use proveno::types::value::LuaValue;
use proveno::{ToolCallRecord, ToolCallStatus, Vm, VmConfig};
use proveno_gateway::config::{DownstreamConfig, Transport};
use proveno_gateway::description;
use proveno_gateway::dialect::compile_program;
use proveno_gateway::downstream::Downstreams;
use proveno_gateway::host::GatewayHost;
use proveno_gateway::policy::Policy;
use proveno_gateway::trace::{CallDecision, Provenance};
use rmcp::model::Tool;
use serde_json::{Value, json};

const POLICY: &str = r#"
[principals.demo-agent]
allow = ["wallet.get_balance", "wallet.transfer", "deep.nested"]

[constraints."wallet.transfer"]
amount_max = 50
"#;

struct Run {
    output: Result<Value, String>,
    log: Vec<(CallDecision, Provenance)>,
    transcript: Vec<ToolCallRecord>,
}

/// Runs `program` as `demo-agent` over the given downstreams, with every
/// discovered tool offered to the prelude and the policy deciding the rest.
async fn run(downstreams: Vec<DownstreamConfig>, program: &'static str) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let policy_path = dir.path().join("policy.toml");
    std::fs::write(&policy_path, POLICY).unwrap();
    let policy = Arc::new(Policy::load(&policy_path).unwrap());
    let downstreams = Arc::new(Downstreams::connect(&downstreams).await.unwrap());
    let allowed: Vec<_> = downstreams
        .tools()
        .iter()
        .filter(|t| {
            policy
                .allowed_tools("demo-agent")
                .contains(&t.qualified_name())
        })
        .cloned()
        .collect();
    let prelude = description::build(&allowed).prelude;
    let host = GatewayHost::new(
        "demo-agent".into(),
        downstreams,
        Arc::new(allowed),
        policy,
        tokio::runtime::Handle::current(),
    );

    tokio::task::spawn_blocking(move || {
        let compiled = compile_program(&prelude, program).unwrap();
        let mut vm = Vm::new(VmConfig::default(), host);
        let output = vm
            .execute(&compiled, LuaValue::Nil)
            .map(|out| {
                serde_json::from_slice(&canonical_serialize(&out.return_value).unwrap()).unwrap()
            })
            .map_err(|e| format!("{e:?}"));
        Run {
            output,
            log: vm.host().log().to_vec(),
            transcript: vm.transcript().to_vec(),
        }
    })
    .await
    .unwrap()
}

fn wallet(url: &str) -> DownstreamConfig {
    DownstreamConfig {
        name: "wallet".into(),
        transport: Transport::Http { url: url.into() },
        credential: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_stage_is_recorded_and_catchable() {
    let mock = common::start_mock_downstream().await;
    let run = run(
        vec![wallet(&mock.url())],
        r#"
local function try(f)
  local ok, err = pcall(f)
  if ok then return "ok" end
  return err
end
local balance = wallet.get_balance{ address = "0x1" }
return {
  balance = balance.usdc,
  over_max = try(function() return wallet.transfer{ to = "0x1", amount = 60 } end),
  not_allowed = try(function() return tool.call("wallet.get_price", { pair = "ETH/USD" }) end),
  bad_args = try(function() return wallet.transfer{ to = "0x1", amount = "ten" } end),
}
"#,
    )
    .await;

    let output = run.output.unwrap();
    assert_eq!(output["balance"], 5000);
    assert_eq!(
        output["over_max"],
        "policy: wallet.transfer: amount 60 exceeds amount_max 50"
    );
    assert_eq!(
        output["not_allowed"],
        "policy: tool wallet.get_price is not allowed for demo-agent"
    );
    let bad_args = output["bad_args"].as_str().unwrap();
    assert!(
        bad_args.starts_with("schema: wallet.transfer: "),
        "{bad_args}"
    );

    let decisions: Vec<&CallDecision> = run.log.iter().map(|(d, _)| d).collect();
    assert!(matches!(decisions[0], CallDecision::Allowed));
    assert!(matches!(
        decisions[1],
        CallDecision::DeniedByPolicy { reason } if reason == "wallet.transfer: amount 60 exceeds amount_max 50"
    ));
    assert!(matches!(decisions[2], CallDecision::DeniedByPolicy { .. }));
    assert!(matches!(
        decisions[3],
        CallDecision::RejectedBySchema { .. }
    ));
    assert!(run.log.iter().all(|(_, p)| *p == Provenance::Unsigned));

    assert_eq!(run.log.len(), run.transcript.len());
    let statuses: Vec<&ToolCallStatus> = run.transcript.iter().map(|r| &r.status).collect();
    assert_eq!(
        statuses,
        [
            &ToolCallStatus::Ok,
            &ToolCallStatus::Error,
            &ToolCallStatus::Error,
            &ToolCallStatus::Error
        ]
    );
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn downstream_error_is_allowed_but_fails_the_call() {
    let mock = common::start_mock_downstream().await;
    // No credential configured, so the mock refuses the transfer.
    let run = run(
        vec![wallet(&mock.url())],
        r#"return tool.call("wallet.transfer", { to = "0x1", amount = 20 })"#,
    )
    .await;

    let error = run.output.unwrap_err();
    assert!(error.contains("downstream: unauthorized"), "{error}");
    assert!(matches!(
        run.log[..],
        [(CallDecision::Allowed, Provenance::Unsigned)]
    ));
    assert_eq!(run.transcript.len(), 1);
    assert_eq!(run.transcript[0].status, ToolCallStatus::Error);
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn response_too_deep_to_record_is_a_recorded_error() {
    let mut deep = json!({ "leaf": 1 });
    for _ in 0..40 {
        deep = json!({ "next": deep });
    }
    let downstream = common::start_fixed_downstream(vec![(
        Tool::new(
            "nested",
            "A deeply nested response.",
            common::open_object_schema(),
        ),
        deep,
    )])
    .await;
    let run = run(
        vec![DownstreamConfig {
            name: "deep".into(),
            transport: Transport::Http {
                url: downstream.url(),
            },
            credential: None,
        }],
        r#"
local ok, err = pcall(function() return deep.nested{} end)
return err
"#,
    )
    .await;

    let error = run.output.unwrap();
    let error = error.as_str().unwrap();
    assert!(
        error.starts_with("downstream: deep.nested: response cannot be recorded"),
        "{error}"
    );
    assert_eq!(run.log.len(), 1);
    assert_eq!(run.transcript.len(), 1);
    assert_eq!(run.transcript[0].status, ToolCallStatus::Error);
    downstream.shutdown().await;
}
