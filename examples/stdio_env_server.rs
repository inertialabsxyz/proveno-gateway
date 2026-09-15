//! A tiny stdio MCP server for tests. Its one tool, `env_keys`, returns the
//! sorted names of the environment variables the process was started with, so
//! a test can check what a downstream child process actually receives.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

#[derive(Clone)]
struct EnvServer;

impl ServerHandler for EnvServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = json!({ "type": "object" }).as_object().unwrap().clone();
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "env_keys",
            "Sorted names of this process's environment variables.",
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name != "env_keys" {
            return Err(ErrorData::invalid_params("unknown tool", None));
        }
        let mut keys: Vec<String> = std::env::vars_os()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        keys.sort();
        Ok(CallToolResult::structured(json!({ "keys": keys })).into())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = EnvServer
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await?;
    service.waiting().await?;
    Ok(())
}
