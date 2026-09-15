//! MCP client connections to the downstream tool servers.
//!
//! One rmcp client connection per configured downstream. Credentials are
//! resolved here at connect time and attached to the transport: as the named
//! environment variable of a stdio child, or as a bearer token on every HTTP
//! request. Nothing above this module ever sees them.

use std::collections::BTreeMap;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::{Deserialize, Serialize};

use crate::config::{DownstreamConfig, Secret, Transport};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Downstream name from config.
    pub server: String,
    /// Tool name as the downstream reports it.
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
}

impl ToolSchema {
    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.server, self.name)
    }
}

pub struct Downstreams {
    clients: BTreeMap<String, RunningService<RoleClient, ()>>,
    tools: Vec<ToolSchema>,
}

#[derive(Debug, thiserror::Error)]
pub enum DownstreamError {
    #[error("{0}")]
    Connect(String),
}

impl Downstreams {
    pub async fn connect(configs: &[DownstreamConfig]) -> Result<Self, DownstreamError> {
        let mut clients = BTreeMap::new();
        let mut tools = Vec::new();
        for config in configs {
            let client = open(config).await?;
            let listed = client.list_all_tools().await.map_err(|e| {
                DownstreamError::Connect(format!("downstream `{}`: tools/list: {e}", config.name))
            })?;
            for tool in listed {
                tools.push(tool_schema(&config.name, tool)?);
            }
            clients.insert(config.name.clone(), client);
        }
        tools.sort_by_key(ToolSchema::qualified_name);
        Ok(Downstreams { clients, tools })
    }

    /// Every discovered tool, sorted by qualified name.
    pub fn tools(&self) -> &[ToolSchema] {
        &self.tools
    }

    pub async fn call(
        &self,
        qualified: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let Some((server, tool)) = qualified.split_once('.') else {
            return Err(format!("unknown tool `{qualified}`"));
        };
        let known = self
            .tools
            .iter()
            .any(|t| t.server == server && t.name == tool);
        let client = match self.clients.get(server) {
            Some(client) if known => client,
            _ => return Err(format!("unknown tool `{qualified}`")),
        };
        let arguments = match args {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            _ => return Err(format!("{qualified}: arguments must be a JSON object")),
        };
        let mut params = CallToolRequestParams::new(tool.to_string());
        params.arguments = arguments;
        let result = client
            .call_tool(params)
            .await
            .map_err(|e| format!("{qualified}: {e}"))?;
        map_result(result)
    }
}

async fn open(
    config: &DownstreamConfig,
) -> Result<RunningService<RoleClient, ()>, DownstreamError> {
    let name = &config.name;
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        DownstreamError::Connect(format!("downstream `{name}`: {what}: {e}"))
    };
    let credential = match &config.credential {
        Some(secret) => Some((
            secret_env_name(secret),
            secret.resolve().map_err(|e| fail("credential", &e))?,
        )),
        None => None,
    };
    match &config.transport {
        Transport::Stdio { command } => {
            let mut parts = command.split_whitespace();
            let program = parts
                .next()
                .ok_or_else(|| fail("spawn", &"command is empty"))?;
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(parts).env_clear();
            for var in ["PATH", "HOME"] {
                if let Some(value) = std::env::var_os(var) {
                    cmd.env(var, value);
                }
            }
            if let Some((var, value)) = credential {
                cmd.env(var, value);
            }
            let transport = TokioChildProcess::new(cmd).map_err(|e| fail("spawn", &e))?;
            ().serve(transport)
                .await
                .map_err(|e| fail("initialize", &e))
        }
        Transport::Http { url } => {
            let mut http = StreamableHttpClientTransportConfig::with_uri(url.as_str());
            if let Some((_, token)) = credential {
                http = http.auth_header(token);
            }
            let transport = StreamableHttpClientTransport::from_config(http);
            ().serve(transport)
                .await
                .map_err(|e| fail("initialize", &e))
        }
    }
}

/// The variable name in a secret's `env:NAME` form.
///
/// `Secret` keeps its reference private and exposes only `resolve`, so the name
/// is read back from its `Debug` form, `Secret("env:NAME")`. Config validation
/// guarantees the `env:` prefix.
fn secret_env_name(secret: &Secret) -> String {
    let debug = format!("{secret:?}");
    let quoted = debug
        .strip_prefix("Secret(")
        .and_then(|s| s.strip_suffix(')'))
        .expect("Secret's Debug form is Secret(\"env:NAME\")");
    let reference: String = serde_json::from_str(quoted).expect("Secret holds a quoted string");
    reference["env:".len()..].to_string()
}

fn tool_schema(server: &str, tool: Tool) -> Result<ToolSchema, DownstreamError> {
    if tool.name.contains('.') {
        return Err(DownstreamError::Connect(format!(
            "downstream `{server}`: tool name `{}` must not contain `.`",
            tool.name
        )));
    }
    Ok(ToolSchema {
        server: server.to_string(),
        name: tool.name.into_owned(),
        description: tool.description.map(|d| d.into_owned()).unwrap_or_default(),
        input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
        output_schema: tool
            .output_schema
            .map(|s| serde_json::Value::Object((*s).clone())),
    })
}

/// Maps a `tools/call` result to the table the host hands back to the VM, as
/// spec section 3.3 step 4 gives it.
fn map_result(result: CallToolResult) -> Result<serde_json::Value, String> {
    let texts: Vec<&str> = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect();
    if result.is_error == Some(true) {
        return Err(texts.concat());
    }
    let value = if let Some(structured) = result.structured_content {
        structured
    } else if let [single] = result.content.as_slice()
        && let Some(text) = single.as_text()
        && let Ok(object @ serde_json::Value::Object(_)) = serde_json::from_str(&text.text)
    {
        object
    } else {
        serde_json::json!({ "text": texts.concat() })
    };
    Ok(match value {
        object @ serde_json::Value::Object(_) => object,
        other => serde_json::json!({ "value": other }),
    })
}

#[cfg(test)]
mod tests {
    use rmcp::model::ContentBlock;
    use serde_json::json;

    use super::*;

    #[test]
    fn qualified_name_prefixes_the_downstream() {
        let tool = ToolSchema {
            server: "wallet".into(),
            name: "transfer".into(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object" }),
            output_schema: None,
        };
        assert_eq!(tool.qualified_name(), "wallet.transfer");
    }

    fn schema(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn tool_schema_copies_fields_and_defaults_description() {
        let mut tool = Tool::new_with_raw("get_price", None, schema(json!({ "type": "object" })));
        tool.output_schema =
            Some(schema(json!({ "type": "object", "required": ["price"] })).into());
        let s = tool_schema("market", tool).unwrap();
        assert_eq!(s.server, "market");
        assert_eq!(s.name, "get_price");
        assert_eq!(s.description, "");
        assert_eq!(s.input_schema, json!({ "type": "object" }));
        assert_eq!(
            s.output_schema,
            Some(json!({ "type": "object", "required": ["price"] }))
        );
    }

    #[test]
    fn dotted_tool_name_is_rejected() {
        let tool = Tool::new("a.b", "x", schema(json!({ "type": "object" })));
        let e = tool_schema("market", tool).unwrap_err().to_string();
        assert!(e.contains("downstream `market`"), "{e}");
        assert!(e.contains("`a.b`"), "{e}");
    }

    #[test]
    fn secret_env_name_reads_the_variable_name() {
        let secret = Secret::try_from("env:WALLET_API_KEY".to_string()).unwrap();
        assert_eq!(secret_env_name(&secret), "WALLET_API_KEY");
    }

    #[test]
    fn is_error_concatenates_text() {
        let result = CallToolResult::error(vec![
            ContentBlock::text("un"),
            ContentBlock::text("authorized"),
        ]);
        assert_eq!(map_result(result), Err("unauthorized".to_string()));
    }

    #[test]
    fn structured_content_wins() {
        let mut result = CallToolResult::success(vec![ContentBlock::text("{\"a\":2}")]);
        result.structured_content = Some(json!({ "a": 1 }));
        assert_eq!(map_result(result), Ok(json!({ "a": 1 })));
    }

    #[test]
    fn non_object_structured_content_is_wrapped() {
        let mut result = CallToolResult::success(vec![]);
        result.structured_content = Some(json!([1, 2]));
        assert_eq!(map_result(result), Ok(json!({ "value": [1, 2] })));
    }

    #[test]
    fn single_json_object_text_is_parsed() {
        let result = CallToolResult::success(vec![ContentBlock::text("{\"price\": 3}")]);
        assert_eq!(map_result(result), Ok(json!({ "price": 3 })));
    }

    #[test]
    fn other_text_is_wrapped() {
        let result = CallToolResult::success(vec![ContentBlock::text("[1]")]);
        assert_eq!(map_result(result), Ok(json!({ "text": "[1]" })));
        let result = CallToolResult::success(vec![
            ContentBlock::text("{\"a\":1}"),
            ContentBlock::text("{\"b\":2}"),
        ]);
        assert_eq!(
            map_result(result),
            Ok(json!({ "text": "{\"a\":1}{\"b\":2}" }))
        );
        let result = CallToolResult::success(vec![]);
        assert_eq!(map_result(result), Ok(json!({ "text": "" })));
    }
}
