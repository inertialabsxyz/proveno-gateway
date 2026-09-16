//! The agent-facing MCP server: `execute`, `check` and the lua-guide.
//!
//! MCP over streamable HTTP, mounted at `/mcp`. Every HTTP request must carry
//! `Authorization: Bearer <token>`; the token names the principal (spec section
//! 8, prototype), and a request without a known token gets 401 before any MCP
//! handling.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, JsonObject, ListPromptsResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, Prompt, PromptMessage, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, Role, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::GatewayConfig;
use crate::dialect::lua_guide;
use crate::engine::{Engine, ExecuteError, ExecuteRequest};

pub const LUA_GUIDE_URI: &str = "proveno://lua-guide";
pub const LUA_GUIDE_NAME: &str = "lua-guide";

const CHECK_DESCRIPTION: &str = "Lint a Lua program against the dialect and your tool API \
without running it. Returns `ok`, or `line N: message` as an error result.";

/// A running server. Dropping it leaves the server running until the runtime
/// ends; call `shutdown` to stop it.
pub struct Server {
    addr: SocketAddr,
    config: StreamableHttpServerConfig,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Server {
    /// The bound address, which differs from `server.listen` when it names
    /// port 0.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn shutdown(self) {
        self.config.cancellation_token.cancel();
        let _ = self.task.await;
    }
}

/// Builds the engine, binds `server.listen` and starts serving in the
/// background.
pub async fn start(config: GatewayConfig) -> anyhow::Result<Server> {
    let tokens = Tokens::resolve(&config)?;
    let listen = config.server.listen.clone();
    let engine = Arc::new(Engine::new(config).await?);

    let http = StreamableHttpServerConfig::default();
    let handler = Gateway { engine };
    let service: StreamableHttpService<Gateway, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Default::default(),
        http.clone(),
    );
    let router = axum::Router::new().nest_service("/mcp", service).layer(
        axum::middleware::from_fn_with_state(Arc::new(tokens), require_bearer),
    );

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("server.listen `{listen}`"))?;
    let addr = listener.local_addr()?;
    let cancel = http.cancellation_token.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { cancel.cancelled_owned().await })
            .await
    });
    Ok(Server {
        addr,
        config: http,
        task,
    })
}

/// Serves until interrupted.
pub async fn serve(config: GatewayConfig) -> anyhow::Result<()> {
    let mut server = start(config).await?;
    eprintln!("proveno-gateway: serving MCP on http://{}/mcp", server.addr);
    tokio::select! {
        result = &mut server.task => {
            result.context("server task")?.context("server")?;
        }
        result = tokio::signal::ctrl_c() => {
            result.context("wait for ctrl-c")?;
            server.shutdown().await;
        }
    }
    Ok(())
}

/// The principal a request authenticated as, set by `require_bearer`.
#[derive(Debug, Clone)]
struct Principal(String);

/// SHA-256 digests of each principal's token. Comparing fixed-length digests
/// keeps the comparison constant time in the token's length as well as its
/// content, and the raw tokens are not kept.
struct Tokens(BTreeMap<String, [u8; 32]>);

impl Tokens {
    fn resolve(config: &GatewayConfig) -> anyhow::Result<Self> {
        let mut digests = BTreeMap::new();
        let mut seen: BTreeMap<[u8; 32], &str> = BTreeMap::new();
        for (name, principal) in &config.principals {
            let token = principal
                .token
                .resolve()
                .with_context(|| format!("principal `{name}` token"))?;
            if token.is_empty() {
                anyhow::bail!("principal `{name}` token: the token is empty");
            }
            let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if let Some(other) = seen.insert(digest, name) {
                anyhow::bail!("principals `{other}` and `{name}` share a token");
            }
            digests.insert(name.clone(), digest);
        }
        Ok(Tokens(digests))
    }

    /// Compares against every principal, without stopping at a match.
    fn principal_for(&self, token: &str) -> Option<&str> {
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for (name, expected) in &self.0 {
            if constant_time_eq(&presented, expected) {
                found = Some(name.as_str());
            }
        }
        found
    }
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

async fn require_bearer(
    State(tokens): State<Arc<Tokens>>,
    mut request: Request,
    next: Next,
) -> Response {
    let principal = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .and_then(|(_, token)| tokens.principal_for(token));
    match principal {
        Some(name) => {
            let principal = Principal(name.to_string());
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
        )
            .into_response(),
    }
}

#[derive(Clone)]
struct Gateway {
    engine: Arc<Engine>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteArgs {
    program: String,
    session: Option<String>,
    request: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckArgs {
    program: String,
}

fn principal(context: &RequestContext<RoleServer>) -> Result<String, ErrorData> {
    context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<Principal>())
        .map(|p| p.0.clone())
        .ok_or_else(|| ErrorData::internal_error("request has no authenticated principal", None))
}

fn args<T: serde::de::DeserializeOwned>(request: &CallToolRequestParams) -> Result<T, ErrorData> {
    let value = serde_json::Value::Object(request.arguments.clone().unwrap_or_default());
    serde_json::from_value(value).map_err(|e| {
        ErrorData::invalid_params(format!("{}: invalid arguments: {e}", request.name), None)
    })
}

fn object_schema(value: serde_json::Value) -> JsonObject {
    match value {
        serde_json::Value::Object(map) => map,
        _ => unreachable!("schema literals are objects"),
    }
}

fn execute_schema() -> JsonObject {
    object_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "program": { "type": "string", "description": "The Lua program to run." },
            "session": { "type": "string", "description": "Groups related executions." },
            "request": { "type": "string", "description": "The task this program serves, recorded in the trace." },
        },
        "required": ["program"],
        "additionalProperties": false,
    }))
}

fn check_schema() -> JsonObject {
    object_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "program": { "type": "string", "description": "The Lua program to lint." },
        },
        "required": ["program"],
        "additionalProperties": false,
    }))
}

impl Gateway {
    async fn execute(
        &self,
        principal: String,
        args: ExecuteArgs,
    ) -> Result<CallToolResult, ErrorData> {
        let request = ExecuteRequest {
            program: args.program,
            session: args.session,
            request: args.request,
        };
        match self.engine.execute(&principal, request).await {
            Ok(response) => {
                let value = serde_json::to_value(&response)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::structured(value))
            }
            // The model wrote the program, so it can fix it and resubmit.
            Err(ExecuteError::Lint(lint)) => Ok(CallToolResult::error(vec![ContentBlock::text(
                lint.to_string(),
            )])),
            // Not something the model can fix, so it is a JSON-RPC error rather
            // than a tool result. The program is not retried: its tool calls may
            // already have been made.
            Err(e @ ExecuteError::Store(_)) => {
                eprintln!("proveno-gateway: execute failed: {e}");
                Err(ErrorData::internal_error(
                    "the trace could not be stored; the program's tool calls may have been made",
                    None,
                ))
            }
        }
    }
}

/// The newest protocol revision this server implements. rmcp 3.3 advertises
/// `2026-07-28` by default but does not emit that revision's `ttlMs` and
/// `cacheScope` cache hints, so a client that negotiates it rejects every
/// `tools/list` result. Advertising up to `2025-11-25` keeps the negotiation
/// on a revision rmcp actually satisfies.
const MAX_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

impl ServerHandler for Gateway {
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(ProtocolVersion::known_up_to(&MAX_PROTOCOL_VERSION))
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let principal = principal(&context)?;
        let description = self.engine.description_for(&principal);
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("execute", description.text, execute_schema()),
            Tool::new("check", CHECK_DESCRIPTION, check_schema()),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let principal = principal(&context)?;
        let result = match request.name.as_ref() {
            "execute" => self.execute(principal, args(&request)?).await?,
            "check" => {
                let CheckArgs { program } = args(&request)?;
                match self.engine.check(&principal, &program) {
                    Ok(()) => CallToolResult::success(vec![ContentBlock::text("ok")]),
                    Err(lint) => CallToolResult::error(vec![ContentBlock::text(lint.to_string())]),
                }
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool `{other}`"),
                    None,
                ));
            }
        };
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(LUA_GUIDE_URI, LUA_GUIDE_NAME)
                .with_description("The Lua dialect guide and example programs.")
                .with_mime_type("text/markdown"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != LUA_GUIDE_URI {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource `{}`", request.uri),
                None,
            ));
        }
        let contents =
            ResourceContents::text(lua_guide(), LUA_GUIDE_URI).with_mime_type("text/markdown");
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            LUA_GUIDE_NAME,
            Some("The Lua dialect guide and example programs."),
            None,
        )]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        if request.name != LUA_GUIDE_NAME {
            return Err(ErrorData::invalid_params(
                format!("unknown prompt `{}`", request.name),
                None,
            ));
        }
        let message = PromptMessage::new_text(Role::User, lua_guide());
        Ok(GetPromptResult::new(vec![message]).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(s: &str) -> [u8; 32] {
        Sha256::digest(s.as_bytes()).into()
    }

    #[test]
    fn token_maps_to_its_principal_only() {
        let tokens = Tokens(BTreeMap::from([
            ("alice".to_string(), digest("a-token")),
            ("bob".to_string(), digest("b-token")),
        ]));
        assert_eq!(tokens.principal_for("a-token"), Some("alice"));
        assert_eq!(tokens.principal_for("b-token"), Some("bob"));
        assert_eq!(tokens.principal_for("a-toke"), None);
        assert_eq!(tokens.principal_for(""), None);
    }
}
