//! Shared test fixtures: an in-process mock downstream MCP server.
//!
//! Extend this module by adding functions; do not change existing ones, other
//! test files depend on them.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

/// The bearer token the mock's `transfer` tool requires.
pub const MOCK_BEARER: &str = "test-secret";

/// Tools per `tools/list` page, small so that discovery must follow cursors.
const PAGE_SIZE: usize = 2;

/// A running mock downstream. Dropping it leaves the server running until the
/// test runtime ends; call `shutdown` to stop it earlier.
pub struct MockDownstream {
    url: String,
    config: StreamableHttpServerConfig,
    server: tokio::task::JoinHandle<()>,
}

impl MockDownstream {
    /// The streamable HTTP endpoint, `http://127.0.0.1:<port>/mcp`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    pub async fn shutdown(self) {
        self.config.cancellation_token.cancel();
        let _ = self.server.await;
    }
}

/// Starts the mock downstream over streamable HTTP on `127.0.0.1:0`. It exposes
/// `get_price`, `get_balance` and `transfer`.
pub async fn start_mock_downstream() -> MockDownstream {
    let config = StreamableHttpServerConfig::default();
    let service: StreamableHttpService<MockServer, LocalSessionManager> =
        StreamableHttpService::new(|| Ok(MockServer), Default::default(), config.clone());
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = config.cancellation_token.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { token.cancelled_owned().await })
            .await;
    });
    MockDownstream {
        url: format!("http://{addr}/mcp"),
        config,
        server,
    }
}

#[derive(Clone)]
struct MockServer;

fn object_schema(properties: Value, required: &[&str]) -> rmcp::model::JsonObject {
    json!({ "type": "object", "properties": properties, "required": required })
        .as_object()
        .unwrap()
        .clone()
}

/// The mock's tools in a fixed order, deliberately not sorted by name.
fn mock_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "transfer",
            "Transfer an amount to an address. Requires a bearer credential.",
            object_schema(
                json!({ "to": { "type": "string" }, "amount": { "type": "integer" } }),
                &["to", "amount"],
            ),
        ),
        Tool::new(
            "get_price",
            "Current and 24h-ago price for a trading pair.",
            object_schema(json!({ "pair": { "type": "string" } }), &["pair"]),
        ),
        Tool::new(
            "get_balance",
            "Balances held by an address.",
            object_schema(json!({ "address": { "type": "string" } }), &["address"]),
        ),
    ]
}

fn arg(request: &CallToolRequestParams, name: &str) -> Value {
    request
        .arguments
        .as_ref()
        .and_then(|a| a.get(name))
        .cloned()
        .unwrap_or(Value::Null)
}

fn has_bearer(context: &RequestContext<RoleServer>) -> bool {
    let expected = format!("Bearer {MOCK_BEARER}");
    context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.headers.get(axum::http::header::AUTHORIZATION))
        .is_some_and(|value| value.as_bytes() == expected.as_bytes())
}

impl ServerHandler for MockServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = mock_tools();
        let start = match request.and_then(|r| r.cursor) {
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| ErrorData::invalid_params("bad cursor", None))?,
            None => 0,
        };
        let end = (start + PAGE_SIZE).min(tools.len());
        let mut result = ListToolsResult::with_all_items(tools[start.min(end)..end].to_vec());
        if end < tools.len() {
            result.next_cursor = Some(end.to_string());
        }
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = match request.name.as_ref() {
            "get_price" => CallToolResult::structured(json!({
                "pair": arg(&request, "pair"),
                "price": 2500.5,
                "price_24h_ago": 2400,
            })),
            "get_balance" => CallToolResult::structured(json!({
                "address": arg(&request, "address"),
                "eth_milli": 3000,
                "usdc": 5000,
            })),
            "transfer" if !has_bearer(&context) => {
                CallToolResult::error(vec![ContentBlock::text("unauthorized")])
            }
            "transfer" => CallToolResult::structured(json!({
                "tx": "0xabc",
                "amount": arg(&request, "amount"),
            })),
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool `{other}`"),
                    None,
                ));
            }
        };
        Ok(result.into())
    }
}

/// Starts a downstream over streamable HTTP whose tools each return a fixed
/// structured response, for shapes the standard mock does not produce.
#[allow(dead_code)] // Each test binary uses a subset of these fixtures.
pub async fn start_fixed_downstream(tools: Vec<(Tool, Value)>) -> MockDownstream {
    let config = StreamableHttpServerConfig::default();
    let server = FixedServer {
        tools: std::sync::Arc::new(tools),
    };
    let service: StreamableHttpService<FixedServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Default::default(),
            config.clone(),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = config.cancellation_token.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { token.cancelled_owned().await })
            .await;
    });
    MockDownstream {
        url: format!("http://{addr}/mcp"),
        config,
        server,
    }
}

/// An object schema with no required properties, for fixed-response tools.
#[allow(dead_code)] // Each test binary uses a subset of these fixtures.
pub fn open_object_schema() -> rmcp::model::JsonObject {
    json!({ "type": "object" }).as_object().unwrap().clone()
}

#[derive(Clone)]
struct FixedServer {
    tools: std::sync::Arc<Vec<(Tool, Value)>>,
}

impl ServerHandler for FixedServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.tools.iter().map(|(tool, _)| tool.clone()).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match self
            .tools
            .iter()
            .find(|(tool, _)| tool.name == request.name)
        {
            Some((_, response)) => Ok(CallToolResult::structured(response.clone()).into()),
            None => Err(ErrorData::invalid_params(
                format!("unknown tool `{}`", request.name),
                None,
            )),
        }
    }
}

/// One tool of an attesting downstream: its fixed structured response and, if
/// any, the object it reports under `proveno/provenance` in the result's `_meta`.
#[allow(dead_code)] // Each test binary uses a subset of these fixtures.
pub struct AttestingTool {
    pub tool: Tool,
    pub response: Value,
    pub provenance: Option<Value>,
}

/// A running attesting downstream and the number of `tools/call` requests it
/// has served, so a test can show a refused call never reached it.
#[allow(dead_code)] // Each test binary uses a subset of these fixtures.
pub struct AttestingDownstream {
    pub downstream: MockDownstream,
    pub calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// Starts a downstream over streamable HTTP whose tools each return a fixed
/// structured response and, when configured, a fixed provenance report in the
/// result's `_meta`.
#[allow(dead_code)] // Each test binary uses a subset of these fixtures.
pub async fn start_attesting_downstream(tools: Vec<AttestingTool>) -> AttestingDownstream {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let config = StreamableHttpServerConfig::default();
    let server = AttestingServer {
        tools: std::sync::Arc::new(tools),
        calls: calls.clone(),
    };
    let service: StreamableHttpService<AttestingServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Default::default(),
            config.clone(),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = config.cancellation_token.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { token.cancelled_owned().await })
            .await;
    });
    AttestingDownstream {
        downstream: MockDownstream {
            url: format!("http://{addr}/mcp"),
            config,
            server,
        },
        calls,
    }
}

#[derive(Clone)]
struct AttestingServer {
    tools: std::sync::Arc<Vec<AttestingTool>>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ServerHandler for AttestingServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.tools.iter().map(|t| t.tool.clone()).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let Some(tool) = self.tools.iter().find(|t| t.tool.name == request.name) else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{}`", request.name),
                None,
            ));
        };
        let mut result = CallToolResult::structured(tool.response.clone());
        if let Some(provenance) = &tool.provenance {
            let mut meta = rmcp::model::MetaObject::new();
            meta.0
                .insert("proveno/provenance".into(), provenance.clone());
            result.meta = Some(meta);
        }
        Ok(result.into())
    }
}
