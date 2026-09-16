//! MCP client connections to the downstream tool servers.
//!
//! One rmcp client connection per configured downstream. Credentials are
//! resolved here at connect time and attached to the transport: as the named
//! environment variable of a stdio child, or as a bearer token on every HTTP
//! request. Nothing above this module ever sees them.
//!
//! A downstream may report where a response came from in the result's `_meta`,
//! under [`PROVENANCE_META_KEY`]. This module reads it and hands it up beside
//! the response, never inside it. It checks only that the report is well
//! formed; it does not verify the claim, and nothing in the gateway does.

use std::collections::BTreeMap;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, MetaObject, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::{Deserialize, Serialize};

use crate::config::{DownstreamConfig, Transport};
use crate::trace::Provenance;

/// The reserved result `_meta` key a downstream reports its provenance under
/// (spec section 3.4).
pub const PROVENANCE_META_KEY: &str = "proveno/provenance";

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

/// A successful `tools/call`, split into what the program sees and what only
/// the trace sees.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResponse {
    /// The value mapped to the program's table. Metadata never reaches it.
    pub value: serde_json::Value,
    /// The tag the downstream reported; `Unsigned` if it reported none.
    pub provenance: Provenance,
    /// The downstream's provenance object as it reported it, as canonical
    /// JSON (keys sorted, no whitespace). Empty for `Unsigned`. The gateway
    /// binds these bytes and does not verify them.
    pub attestation: Vec<u8>,
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
    ) -> Result<ToolResponse, String> {
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
        let mut result = client
            .call_tool(params)
            .await
            .map_err(|e| format!("{qualified}: {e}"))?;
        let meta = result.meta.take();
        let value = map_result(result)?;
        let (provenance, attestation) = read_provenance(meta.as_ref()).map_err(|e| {
            format!("{qualified}: malformed provenance from downstream `{server}`: {e}")
        })?;
        Ok(ToolResponse {
            value,
            provenance,
            attestation,
        })
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
            secret.env_name(),
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

/// Reads the provenance a downstream reported in a successful result's
/// `_meta`. No report is `Unsigned` with no attestation. A report that is not
/// exactly one of the four tags with that tag's fields is an error, never a
/// quiet `Unsigned`: a server that means to attest and gets it wrong should
/// hear about it.
///
/// Returns the typed tag and the attestation blob: the reported object,
/// verbatim, as canonical JSON. Only its shape is checked here.
fn read_provenance(meta: Option<&MetaObject>) -> Result<(Provenance, Vec<u8>), String> {
    let Some(reported) = meta.and_then(|m| m.get(PROVENANCE_META_KEY)) else {
        return Ok((Provenance::Unsigned, Vec::new()));
    };
    if !reported.is_object() {
        return Err(format!("`{PROVENANCE_META_KEY}` must be an object"));
    }
    let provenance: Provenance = serde_json::from_value(reported.clone())
        .map_err(|e| format!("`{PROVENANCE_META_KEY}`: {e}"))?;
    let typed = serde_json::to_value(&provenance).expect("provenance serializes to JSON");
    let (serde_json::Value::Object(reported_fields), serde_json::Value::Object(typed_fields)) =
        (reported, &typed)
    else {
        unreachable!("both are objects");
    };
    let mut unknown: Vec<&str> = reported_fields
        .keys()
        .filter(|k| !typed_fields.contains_key(*k))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(format!(
            "`{PROVENANCE_META_KEY}`: `{}` does not define field(s) {}",
            typed_fields["type"].as_str().unwrap_or_default(),
            unknown
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some((field, _)) = typed_fields.iter().find(|(_, v)| v.as_str() == Some("")) {
        return Err(format!("`{PROVENANCE_META_KEY}`: field `{field}` is empty"));
    }
    let attestation = match provenance {
        Provenance::Unsigned => Vec::new(),
        _ => canonical_json(reported_fields),
    };
    Ok((provenance, attestation))
}

/// JSON with keys sorted and no whitespace, so the same report gives the same
/// bytes whatever order the downstream wrote its keys in. Only called on a
/// report that has passed `read_provenance`'s checks, whose values are all
/// strings and unsigned integers.
fn canonical_json(fields: &serde_json::Map<String, serde_json::Value>) -> Vec<u8> {
    let sorted: BTreeMap<&String, &serde_json::Value> = fields.iter().collect();
    serde_json::to_vec(&sorted).expect("strings and integers serialize")
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

    fn meta(provenance: serde_json::Value) -> MetaObject {
        let mut meta = MetaObject::new();
        meta.0.insert(PROVENANCE_META_KEY.into(), provenance);
        meta
    }

    #[test]
    fn missing_provenance_is_unsigned_with_no_attestation() {
        assert_eq!(
            read_provenance(None),
            Ok((Provenance::Unsigned, Vec::new()))
        );
        let mut other = MetaObject::new();
        other.set_traceparent("00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01");
        assert_eq!(
            read_provenance(Some(&other)),
            Ok((Provenance::Unsigned, Vec::new()))
        );
    }

    #[test]
    fn reported_provenance_maps_to_each_variant() {
        let cases = [
            (json!({ "type": "unsigned" }), Provenance::Unsigned, ""),
            (
                json!({ "type": "signed", "by": "feed-key-1", "sig": "0xabcd" }),
                Provenance::Signed {
                    by: "feed-key-1".into(),
                    sig: "0xabcd".into(),
                },
                r#"{"by":"feed-key-1","sig":"0xabcd","type":"signed"}"#,
            ),
            (
                json!({ "type": "onchain", "chain": "31337", "block": 12, "reference": "0xbeef" }),
                Provenance::Onchain {
                    chain: "31337".into(),
                    block: 12,
                    reference: "0xbeef".into(),
                },
                r#"{"block":12,"chain":"31337","reference":"0xbeef","type":"onchain"}"#,
            ),
            (
                json!({ "type": "notarized", "scheme": "tlsnotary", "reference": "sha256:00" }),
                Provenance::Notarized {
                    scheme: "tlsnotary".into(),
                    reference: "sha256:00".into(),
                },
                r#"{"reference":"sha256:00","scheme":"tlsnotary","type":"notarized"}"#,
            ),
        ];
        for (reported, tag, blob) in cases {
            assert_eq!(
                read_provenance(Some(&meta(reported.clone()))),
                Ok((tag, blob.as_bytes().to_vec())),
                "{reported}"
            );
        }
    }

    #[test]
    fn provenance_attestation_ignores_reported_key_order() {
        let a: serde_json::Value = serde_json::from_str(
            r#"{"type":"onchain","reference":"0xbeef","block":12,"chain":"31337"}"#,
        )
        .unwrap();
        let b: serde_json::Value = serde_json::from_str(
            r#"{ "chain": "31337", "block": 12, "type": "onchain", "reference": "0xbeef" }"#,
        )
        .unwrap();
        let (_, blob_a) = read_provenance(Some(&meta(a))).unwrap();
        let (_, blob_b) = read_provenance(Some(&meta(b))).unwrap();
        assert_eq!(blob_a, blob_b);
    }

    #[test]
    fn malformed_provenance_is_an_error_not_unsigned() {
        let cases = [
            (json!(null), "must be an object"),
            (json!("onchain"), "must be an object"),
            (json!({}), "missing field `type`"),
            (json!({ "type": "gossip" }), "unknown variant `gossip`"),
            (
                json!({ "type": "onchain", "chain": "1", "block": 12 }),
                "missing field `reference`",
            ),
            (
                json!({ "type": "onchain", "chain": "1", "block": "12", "reference": "0x1" }),
                "invalid type",
            ),
            (
                json!({ "type": "onchain", "chain": "1", "block": -1, "reference": "0x1" }),
                "invalid value",
            ),
            (
                json!({ "type": "onchain", "chain": "1", "block": 12, "reference": "0x1", "proof": "0x2" }),
                "`onchain` does not define field(s) `proof`",
            ),
            (
                json!({ "type": "unsigned", "sig": "0x1" }),
                "`unsigned` does not define field(s) `sig`",
            ),
            (
                json!({ "type": "signed", "by": "", "sig": "0x1" }),
                "field `by` is empty",
            ),
        ];
        for (reported, expected) in cases {
            let e = read_provenance(Some(&meta(reported.clone()))).unwrap_err();
            assert!(e.contains(PROVENANCE_META_KEY), "{reported}: {e}");
            assert!(e.contains(expected), "{reported}: {e}");
        }
    }

    #[test]
    fn provenance_metadata_never_reaches_the_value() {
        let mut result = CallToolResult::structured(json!({ "eth_milli": 3 }));
        result.meta = Some(meta(
            json!({ "type": "onchain", "chain": "1", "block": 12, "reference": "0x1" }),
        ));
        assert_eq!(map_result(result), Ok(json!({ "eth_milli": 3 })));
    }
}
