//! A minimal MCP client for the proveno-gateway demo.
//!
//! It stands in for "any agent": it connects to the gateway over streamable
//! HTTP with a bearer token, and either runs a Lua program through `execute` or
//! prints the tool description the gateway generated for this principal. Any
//! other MCP client can do the same; see demo/README.md.

use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};

/// The environment variable holding this principal's bearer token.
const TOKEN_VAR: &str = "DEMO_AGENT_TOKEN";

#[derive(Parser)]
#[command(
    name = "demo-client",
    about = "MCP client for the proveno-gateway demo"
)]
struct Cli {
    /// The gateway's MCP endpoint.
    #[arg(long, default_value = "http://127.0.0.1:7777/mcp")]
    url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a Lua program through the gateway and print the structured result.
    Execute {
        file: std::path::PathBuf,
        /// The natural-language task the program serves, recorded in the trace.
        #[arg(long)]
        request: Option<String>,
    },
    /// Print the description of the gateway's `execute` tool.
    Description,
}

async fn connect(url: &str) -> anyhow::Result<RunningService<RoleClient, ()>> {
    let token = std::env::var(TOKEN_VAR)
        .map_err(|_| anyhow::anyhow!("{TOKEN_VAR} is not set; it is the gateway bearer token"))?;
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(token);
    Ok(
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await?,
    )
}

async fn execute(
    client: &RunningService<RoleClient, ()>,
    program: String,
    request: Option<String>,
) -> anyhow::Result<()> {
    let mut args = json!({ "program": program });
    if let Some(request) = request {
        args["request"] = json!(request);
    }
    let mut params = CallToolRequestParams::new("execute".to_string());
    params.arguments = Some(args.as_object().expect("an object").clone());

    let result = client.call_tool(params).await?;
    let text: String = result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.as_str())
        .collect();
    if result.is_error == Some(true) {
        // A lint error: the program never ran, so there is no trace.
        anyhow::bail!("{text}");
    }
    let value = result.structured_content.unwrap_or(Value::String(text));
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

async fn description(client: &RunningService<RoleClient, ()>) -> anyhow::Result<()> {
    let tool = client
        .list_all_tools()
        .await?
        .into_iter()
        .find(|tool| tool.name == "execute")
        .ok_or_else(|| anyhow::anyhow!("the gateway exposes no `execute` tool"))?;
    println!("{}", tool.description.unwrap_or_default());
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let client = connect(&cli.url).await?;
    let result = match cli.command {
        Command::Execute { file, request } => {
            let program = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
            execute(&client, program, request).await
        }
        Command::Description => description(&client).await,
    };
    client.cancel().await?;
    result
}
