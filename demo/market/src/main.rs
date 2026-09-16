//! A price MCP server over streamable HTTP, for the proveno-gateway demo.
//!
//! Prices come from a JSON fixture so the demo is reproducible. `price` is a
//! decimal, which the gateway maps to a decimal string at the host boundary
//! because the VM has no floats.
//!
//! If `MARKET_TOKEN` is set, every request must carry it as
//! `Authorization: Bearer`. That is the credential the gateway injects for an
//! http downstream.

use std::collections::BTreeMap;
use std::sync::Arc;

use clap::Parser;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "demo-market",
    about = "Price MCP server for the proveno-gateway demo"
)]
struct Cli {
    /// Address to serve `/mcp` on.
    #[arg(long, default_value = "127.0.0.1:8081")]
    listen: String,
    /// JSON file of pairs to `{ price, price_24h_ago }`.
    #[arg(long)]
    fixture: std::path::PathBuf,
}

/// The fixture: pair to the object `get_price` returns for it.
type Prices = BTreeMap<String, Value>;

#[derive(Clone)]
struct Market {
    prices: Arc<Prices>,
}

fn object_schema(properties: Value, required: &[&str]) -> rmcp::model::JsonObject {
    json!({ "type": "object", "properties": properties, "required": required })
        .as_object()
        .expect("schema literals are objects")
        .clone()
}

fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "get_price",
            "Current and 24h-ago price for a trading pair.",
            object_schema(
                json!({ "pair": { "type": "string", "description": "Such as ETH/USD." } }),
                &["pair"],
            ),
        )
        .with_raw_output_schema(
            object_schema(
                json!({
                    "pair": { "type": "string" },
                    "price": { "type": "number" },
                    "price_24h_ago": { "type": "number" },
                }),
                &["pair", "price", "price_24h_ago"],
            )
            .into(),
        ),
    ]
}

impl ServerHandler for Market {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name != "get_price" {
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{}`", request.name),
                None,
            ));
        }
        let pair = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("pair"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = match self.prices.get(pair) {
            Some(quote) => {
                let mut response = quote.clone();
                response["pair"] = json!(pair);
                CallToolResult::structured(response)
            }
            None => {
                CallToolResult::error(vec![ContentBlock::text(format!("unknown pair `{pair}`"))])
            }
        };
        Ok(result.into())
    }
}

/// Rejects every request that does not carry the expected bearer token.
async fn require_bearer(
    axum::extract::State(expected): axum::extract::State<Arc<String>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if presented == Some(expected.as_str()) {
        next.run(request).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let text = std::fs::read_to_string(&cli.fixture)
        .map_err(|e| anyhow::anyhow!("{}: {e}", cli.fixture.display()))?;
    let prices: Prices = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{}: {e}", cli.fixture.display()))?;

    let market = Market {
        prices: Arc::new(prices),
    };
    let config = StreamableHttpServerConfig::default();
    let service: StreamableHttpService<Market, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(market.clone()), Default::default(), config);
    let mut router = axum::Router::new().nest_service("/mcp", service);
    if let Ok(token) = std::env::var("MARKET_TOKEN") {
        router = router.layer(axum::middleware::from_fn_with_state(
            Arc::new(token),
            require_bearer,
        ));
    }

    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    eprintln!("demo-market: serving MCP on http://{}/mcp", cli.listen);
    axum::serve(listener, router).await?;
    Ok(())
}
