//! The agent-facing MCP server end to end: the mock downstream, the gateway on
//! `127.0.0.1:0`, and an rmcp streamable HTTP client with a bearer token.

mod common;

use std::sync::LazyLock;

use proveno_gateway::config::{self, GatewayConfig};
use proveno_gateway::dialect::lua_guide;
use proveno_gateway::server::{self, Server};
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, GetPromptRequestParams, ReadResourceRequestParams,
    ResourceContents,
};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const WALLET_CREDENTIAL_VAR: &str = "PROVENO_GATEWAY_SERVER_TEST_WALLET_KEY";
const SIGNING_KEY_VAR: &str = "PROVENO_GATEWAY_SERVER_TEST_SIGNING_KEY";
const AGENT_TOKEN_VAR: &str = "PROVENO_GATEWAY_SERVER_TEST_AGENT_TOKEN";
const READER_TOKEN_VAR: &str = "PROVENO_GATEWAY_SERVER_TEST_READER_TOKEN";
const EMPTY_TOKEN_VAR: &str = "PROVENO_GATEWAY_SERVER_TEST_EMPTY_TOKEN";
const SIGNING_KEY_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";
const AGENT_TOKEN: &str = "demo-agent-token";
const READER_TOKEN: &str = "reader-token";

/// Sets the secret variables once, before any test reads the environment.
/// Every test calls this first, so no test thread reads the environment while
/// it is being written.
static ENV: LazyLock<()> = LazyLock::new(|| {
    // SAFETY: the LazyLock runs this exactly once, and every test in this
    // binary forces it before touching the environment or spawning anything.
    unsafe {
        std::env::set_var(WALLET_CREDENTIAL_VAR, common::MOCK_BEARER);
        std::env::set_var(SIGNING_KEY_VAR, SIGNING_KEY_HEX);
        std::env::set_var(AGENT_TOKEN_VAR, AGENT_TOKEN);
        std::env::set_var(READER_TOKEN_VAR, READER_TOKEN);
        std::env::set_var(EMPTY_TOKEN_VAR, "");
    }
});

/// `demo-agent` may transfer; `reader` may only read.
const POLICY: &str = r#"
[principals.demo-agent]
allow = ["wallet.get_balance", "market.get_price", "wallet.transfer"]

[principals.reader]
allow = ["wallet.get_balance", "market.get_price"]
"#;

const PRINCIPALS: &str = "[principals.demo-agent]\ntoken = \"env:PROVENO_GATEWAY_SERVER_TEST_AGENT_TOKEN\"\n\
[principals.reader]\ntoken = \"env:PROVENO_GATEWAY_SERVER_TEST_READER_TOKEN\"\n";

/// A gateway config over the mock downstream listening on `127.0.0.1:0`,
/// written to a file so relative paths resolve as in production. `principals`
/// is the `[principals.*]` TOML.
fn gateway_config(url: &str, principals: &str) -> (GatewayConfig, tempfile::TempDir) {
    LazyLock::force(&ENV);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("policy.toml"), POLICY).unwrap();
    let text = format!(
        r#"
[server]
listen = "127.0.0.1:0"
signing_key = "env:{SIGNING_KEY_VAR}"

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

{principals}
"#
    );
    std::fs::write(dir.path().join("gateway.toml"), text).unwrap();
    let config = config::load(&dir.path().join("gateway.toml")).unwrap();
    (config, dir)
}

async fn connect(server: &Server, token: &str) -> RunningService<RoleClient, ()> {
    try_connect(server, token).await.unwrap()
}

async fn try_connect(
    server: &Server,
    token: &str,
) -> Result<RunningService<RoleClient, ()>, Box<rmcp::service::ClientInitializeError>> {
    let url = format!("http://{}/mcp", server.local_addr());
    let http = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token);
    ().serve(StreamableHttpClientTransport::from_config(http))
        .await
        .map_err(Box::new)
}

async fn call(
    client: &RunningService<RoleClient, ()>,
    tool: &str,
    args: Value,
) -> Result<CallToolResult, ServiceError> {
    let mut params = CallToolRequestParams::new(tool.to_string());
    params.arguments = Some(args.as_object().unwrap().clone());
    client.call_tool(params).await
}

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .collect()
}

/// Sends a bare `POST /mcp` and returns the HTTP status code.
async fn post_status(server: &Server, authorization: Option<&str>) -> u16 {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(server.local_addr())
        .await
        .unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response);
    response
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("not an HTTP response: {response}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_are_execute_and_check_described_per_principal() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), PRINCIPALS);
    let server = server::start(config).await.unwrap();

    let agent = connect(&server, AGENT_TOKEN).await;
    let tools = agent.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(names, ["execute", "check"]);
    let execute = tools[0].description.as_deref().unwrap();
    assert!(execute.contains("wallet.transfer{"), "{execute}");

    let reader = connect(&server, READER_TOKEN).await;
    let tools = reader.list_all_tools().await.unwrap();
    let execute = tools[0].description.as_deref().unwrap();
    assert!(!execute.contains("wallet.transfer"), "{execute}");
    assert!(execute.contains("wallet.get_balance{"), "{execute}");

    agent.cancel().await.unwrap();
    reader.cancel().await.unwrap();
    server.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_returns_the_trace_id_and_status() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), PRINCIPALS);
    let store_dir = config.store.dir.clone();
    let server = server::start(config).await.unwrap();
    let client = connect(&server, AGENT_TOKEN).await;

    let program = r#"
local b = wallet.get_balance{ address = "0x1" }
return b.usdc
"#;
    let result = call(
        &client,
        "execute",
        json!({ "program": program, "session": "s-1", "request": "read the balance" }),
    )
    .await
    .unwrap();
    assert_ne!(result.is_error, Some(true), "{}", text(&result));
    let structured = result.structured_content.clone().unwrap();
    assert_eq!(structured["result"], json!(5000));
    assert_eq!(structured["status"], json!({ "type": "ok" }));
    let trace_id = structured["trace_id"].as_str().unwrap();
    assert!(
        store_dir
            .join("traces")
            .join(format!("{trace_id}.json"))
            .exists()
    );
    let content: Value = serde_json::from_str(&text(&result)).unwrap();
    assert_eq!(content, structured);

    // A run that starts and fails is still a result, with its trace.
    let failed = call(
        &client,
        "execute",
        json!({ "program": "local t = wallet.transfer{ to = \"0x1\", amount = \"x\" }\nreturn t" }),
    )
    .await
    .unwrap();
    let structured = failed.structured_content.clone().unwrap();
    assert_eq!(structured["status"]["type"], "error");
    assert!(structured["trace_id"].is_string());

    client.cancel().await.unwrap();
    server.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lint_error_is_a_tool_error_with_the_line() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), PRINCIPALS);
    let server = server::start(config).await.unwrap();
    let client = connect(&server, AGENT_TOKEN).await;

    let program = "local a = 1\nreturn os.time()\n";
    for tool in ["execute", "check"] {
        let result = call(&client, tool, json!({ "program": program }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true), "{tool}");
        let message = text(&result);
        assert!(message.starts_with("line 2: "), "{tool}: {message}");
        assert!(message.contains("os"), "{tool}: {message}");
    }

    let ok = call(&client, "check", json!({ "program": "return 1" }))
        .await
        .unwrap();
    assert_ne!(ok.is_error, Some(true));
    assert_eq!(text(&ok), "ok");

    client.cancel().await.unwrap();
    server.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unwritable_store_is_an_mcp_error_and_the_server_keeps_serving() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), PRINCIPALS);
    let traces = config.store.dir.join("traces");
    let server = server::start(config).await.unwrap();
    let client = connect(&server, AGENT_TOKEN).await;

    std::fs::remove_dir_all(&traces).unwrap();
    std::fs::write(&traces, "not a directory").unwrap();

    let err = call(&client, "execute", json!({ "program": "return 1" }))
        .await
        .unwrap_err();
    let ServiceError::McpError(error) = err else {
        panic!("expected a JSON-RPC error, got {err:?}");
    };
    assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
    assert!(
        error.message.contains("trace could not be stored"),
        "{}",
        error.message
    );

    let ok = call(&client, "check", json!({ "program": "return 1" }))
        .await
        .unwrap();
    assert_eq!(text(&ok), "ok");
    assert_eq!(client.list_all_tools().await.unwrap().len(), 2);

    client.cancel().await.unwrap();
    server.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lua_guide_is_a_resource_and_a_prompt() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), PRINCIPALS);
    let server = server::start(config).await.unwrap();
    let client = connect(&server, READER_TOKEN).await;

    let resources = client.list_all_resources().await.unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "proveno://lua-guide");
    assert_eq!(resources[0].mime_type.as_deref(), Some("text/markdown"));

    let read = client
        .read_resource(ReadResourceRequestParams::new("proveno://lua-guide"))
        .await
        .unwrap();
    let [
        ResourceContents::TextResourceContents {
            mime_type, text, ..
        },
    ] = &read.contents[..]
    else {
        panic!("expected one text resource, got {:?}", read.contents);
    };
    assert_eq!(mime_type.as_deref(), Some("text/markdown"));
    assert_eq!(*text, lua_guide());

    let prompts = client.list_all_prompts().await.unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "lua-guide");
    let prompt = client
        .get_prompt(GetPromptRequestParams::new("lua-guide"))
        .await
        .unwrap();
    assert_eq!(prompt.messages.len(), 1);
    assert_eq!(
        prompt.messages[0]
            .content
            .as_text()
            .map(|t| t.text.as_str()),
        Some(lua_guide().as_str())
    );

    client.cancel().await.unwrap();
    server.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_or_wrong_token_gets_401() {
    let mock = common::start_mock_downstream().await;
    let (config, _dir) = gateway_config(&mock.url(), PRINCIPALS);
    let server = server::start(config).await.unwrap();

    assert_eq!(post_status(&server, None).await, 401);
    assert_eq!(post_status(&server, Some("Bearer wrong-token")).await, 401);
    assert_eq!(post_status(&server, Some("Bearer ")).await, 401);
    assert_eq!(post_status(&server, Some(AGENT_TOKEN)).await, 401);
    assert_eq!(
        post_status(&server, Some(&format!("Basic {AGENT_TOKEN}"))).await,
        401
    );
    assert_ne!(
        post_status(&server, Some(&format!("Bearer {AGENT_TOKEN}"))).await,
        401
    );
    assert!(try_connect(&server, "wrong-token").await.is_err());

    server.shutdown().await;
    mock.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_or_shared_tokens_fail_at_startup() {
    let mock = common::start_mock_downstream().await;

    let empty = format!("{PRINCIPALS}[principals.nobody]\ntoken = \"env:{EMPTY_TOKEN_VAR}\"\n");
    let (config, _dir) = gateway_config(&mock.url(), &empty);
    let err = server::start(config).await.err().unwrap().to_string();
    assert!(err.contains("principal `nobody`"), "{err}");

    let shared = format!("{PRINCIPALS}[principals.twin]\ntoken = \"env:{AGENT_TOKEN_VAR}\"\n");
    let (config, _dir) = gateway_config(&mock.url(), &shared);
    let err = server::start(config).await.err().unwrap().to_string();
    assert!(err.contains("share a token"), "{err}");

    mock.shutdown().await;
}
