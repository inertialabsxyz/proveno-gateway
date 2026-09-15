//! The downstream MCP client against real rmcp connections: the in-process mock
//! over streamable HTTP, and a stdio child built from `examples/`.

mod common;

use std::sync::LazyLock;

use proveno_gateway::config::{DownstreamConfig, Secret, Transport};
use proveno_gateway::downstream::Downstreams;
use serde_json::json;

const HTTP_CREDENTIAL_VAR: &str = "PROVENO_GATEWAY_TEST_MARKET_KEY";
const STDIO_CREDENTIAL_VAR: &str = "PROVENO_GATEWAY_TEST_STDIO_KEY";

/// Sets the credential variables once, before any test reads the environment.
/// Every test calls this first, so no test thread reads the environment while
/// it is being written.
static ENV: LazyLock<()> = LazyLock::new(|| {
    // SAFETY: the LazyLock runs this exactly once, and every test in this
    // binary forces it before touching the environment or spawning anything.
    unsafe {
        std::env::set_var(HTTP_CREDENTIAL_VAR, common::MOCK_BEARER);
        std::env::set_var(STDIO_CREDENTIAL_VAR, "stdio-secret");
    }
});

fn http(name: &str, url: &str, credential: Option<&str>) -> DownstreamConfig {
    LazyLock::force(&ENV);
    DownstreamConfig {
        name: name.into(),
        transport: Transport::Http { url: url.into() },
        credential: credential.map(|var| Secret::try_from(format!("env:{var}")).unwrap()),
    }
}

#[tokio::test]
async fn discovery_is_sorted_by_qualified_name() {
    let mock = common::start_mock_downstream().await;
    let downstreams = Downstreams::connect(&[
        http("wallet", &mock.url(), None),
        http("market", &mock.url(), None),
    ])
    .await
    .unwrap();

    let names: Vec<String> = downstreams
        .tools()
        .iter()
        .map(|t| t.qualified_name())
        .collect();
    // The mock lists two tools per page, so all three means pagination was followed.
    assert_eq!(
        names,
        [
            "market.get_balance",
            "market.get_price",
            "market.transfer",
            "wallet.get_balance",
            "wallet.get_price",
            "wallet.transfer",
        ]
    );
    let price = &downstreams.tools()[1];
    assert_eq!(price.server, "market");
    assert_eq!(price.name, "get_price");
    assert!(!price.description.is_empty());
    assert_eq!(price.input_schema["required"], json!(["pair"]));
    assert_eq!(price.output_schema, None);
    mock.shutdown().await;
}

#[tokio::test]
async fn call_returns_structured_content() {
    let mock = common::start_mock_downstream().await;
    let downstreams = Downstreams::connect(&[http("market", &mock.url(), None)])
        .await
        .unwrap();
    let result = downstreams
        .call("market.get_price", json!({ "pair": "ETH/USDC" }))
        .await
        .unwrap();
    // Numbers pass through untouched; mapping to integers is values.rs's job.
    assert_eq!(
        result,
        json!({ "pair": "ETH/USDC", "price": 2500.5, "price_24h_ago": 2400 })
    );
}

#[tokio::test]
async fn transfer_requires_the_bearer_credential() {
    let mock = common::start_mock_downstream().await;
    let downstreams =
        Downstreams::connect(&[http("wallet", &mock.url(), Some(HTTP_CREDENTIAL_VAR))])
            .await
            .unwrap();
    let result = downstreams
        .call("wallet.transfer", json!({ "to": "0x1", "amount": 40 }))
        .await
        .unwrap();
    assert_eq!(result, json!({ "tx": "0xabc", "amount": 40 }));
}

#[tokio::test]
async fn is_error_maps_to_err_with_its_text() {
    let mock = common::start_mock_downstream().await;
    let downstreams = Downstreams::connect(&[http("wallet", &mock.url(), None)])
        .await
        .unwrap();
    let err = downstreams
        .call("wallet.transfer", json!({ "to": "0x1", "amount": 40 }))
        .await
        .unwrap_err();
    assert_eq!(err, "unauthorized");
}

#[tokio::test]
async fn unknown_tool_or_server_is_err() {
    let mock = common::start_mock_downstream().await;
    let downstreams = Downstreams::connect(&[http("market", &mock.url(), None)])
        .await
        .unwrap();
    for qualified in ["market.no_such_tool", "wallet.get_price", "get_price", ""] {
        let err = downstreams.call(qualified, json!({})).await.unwrap_err();
        assert!(err.contains("unknown tool"), "{qualified}: {err}");
    }
}

#[tokio::test]
async fn unset_credential_fails_connect_naming_the_downstream() {
    let mock = common::start_mock_downstream().await;
    let config = http("market", &mock.url(), Some("PROVENO_GATEWAY_TEST_UNSET"));
    let err = Downstreams::connect(&[config]).await.err().unwrap();
    let message = err.to_string();
    assert!(message.contains("downstream `market`"), "{message}");
    assert!(message.contains("PROVENO_GATEWAY_TEST_UNSET"), "{message}");
}

#[tokio::test]
async fn unreachable_downstream_fails_connect_naming_it() {
    LazyLock::force(&ENV);
    let mock = common::start_mock_downstream().await;
    let url = mock.url();
    mock.shutdown().await;
    let err = Downstreams::connect(&[http("market", &url, None)])
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("downstream `market`"), "{err}");
}

#[test]
fn downstreams_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Downstreams>();
}

/// Builds `examples/stdio_env_server.rs` and returns the binary's path.
/// `cargo test --test downstream` does not build examples on its own.
fn stdio_env_server() -> std::path::PathBuf {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--example", "stdio_env_server"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .unwrap();
    assert!(status.success(), "building the stdio_env_server example");
    // Test binaries live in target/<profile>/deps; examples in target/<profile>/examples.
    let profile_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    profile_dir
        .join("examples")
        .join(format!("stdio_env_server{}", std::env::consts::EXE_SUFFIX))
}

#[tokio::test]
async fn stdio_child_gets_only_path_home_and_credential() {
    LazyLock::force(&ENV);
    let binary = stdio_env_server();
    let config = DownstreamConfig {
        name: "env".into(),
        transport: Transport::Stdio {
            command: binary.to_str().unwrap().into(),
        },
        credential: Some(Secret::try_from(format!("env:{STDIO_CREDENTIAL_VAR}")).unwrap()),
    };
    let downstreams = Downstreams::connect(&[config]).await.unwrap();
    let result = downstreams.call("env.env_keys", json!({})).await.unwrap();
    let keys: Vec<&str> = result["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .filter(|k| !OS_INJECTED.contains(k))
        .collect();

    let mut expected = vec![STDIO_CREDENTIAL_VAR];
    for var in ["HOME", "PATH"] {
        if std::env::var_os(var).is_some() {
            expected.push(var);
        }
    }
    expected.sort();
    assert_eq!(keys, expected);
}

/// Variables the OS may add to a process started with a cleared environment.
/// macOS can set `__CF_USER_TEXT_ENCODING` for CoreFoundation; Linux adds none.
const OS_INJECTED: &[&str] = &["__CF_USER_TEXT_ENCODING"];
