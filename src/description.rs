//! The generated tool description and the Lua prelude that goes with it.
//!
//! Both are a pure function of `DIALECT_VERSION` and the allowed tool schemas:
//! tools and properties are sorted, and nothing depends on time or on map
//! iteration order.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::dialect::dialect_rules;
use crate::downstream::ToolSchema;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescription {
    pub text: String,
    pub hash: [u8; 32],
    pub prelude: String,
}

/// The placeholder tool the examples are written against, as `(server, tool)`.
pub const EXAMPLE_TOOL: (&str, &str) = ("example", "lookup");

/// Fixed example programs. They use a placeholder tool, not the real allow-list,
/// so the text stays stable.
pub const EXAMPLES: [&str; 3] = [
    // Calling a tool with table-call sugar and returning a field.
    "\
local item = example.lookup{ id = \"a1\" }
return item.status",
    // Catching a denied or failed call and reporting it.
    "\
local ok, res = pcall(function()
  return example.lookup{ id = \"a1\" }
end)
if not ok then
  return { ok = false, error = res }
end
return { ok = true, status = res.status }",
    // Several calls, returning a table as the result.
    "\
local ids = { \"a1\", \"b2\" }
local statuses = {}
for _, id in ipairs(ids) do
  local item = example.lookup{ id = id }
  statuses[id] = item.status
end
return { count = #ids, statuses = statuses }",
];

/// The examples section of the description and of the lua-guide.
pub fn examples_section() -> String {
    let (server, tool) = EXAMPLE_TOOL;
    let mut out = format!(
        "# Examples\n\n`{server}.{tool}{{ id: string }} -> table` is a placeholder; \
         use the tools listed in the tool API.\n"
    );
    for example in EXAMPLES {
        out.push_str("\n```lua\n");
        out.push_str(example);
        out.push_str("\n```\n");
    }
    out
}

pub fn build(allowed: &[ToolSchema]) -> ToolDescription {
    let mut tools: Vec<&ToolSchema> = allowed.iter().collect();
    tools.sort_by_key(|t| t.qualified_name());

    let mut text = String::new();
    text.push_str(dialect_rules());
    text.push_str("\n# Tool API\n\n");
    if tools.is_empty() {
        text.push_str("No tools are available.\n");
    }
    for tool in &tools {
        text.push_str(&render_tool(tool));
    }
    text.push('\n');
    text.push_str(&examples_section());

    let hash = Sha256::digest(text.as_bytes()).into();
    ToolDescription {
        text,
        hash,
        prelude: prelude(&tools),
    }
}

fn prelude(sorted_tools: &[&ToolSchema]) -> String {
    let mut servers: Vec<&str> = sorted_tools.iter().map(|t| t.server.as_str()).collect();
    servers.sort_unstable();
    servers.dedup();

    let mut out = String::new();
    for server in servers {
        out.push_str(&format!("local {server} = {{}}\n"));
    }
    for tool in sorted_tools {
        let q = tool.qualified_name();
        out.push_str(&format!(
            "{q} = function(args) return tool.call(\"{q}\", args) end\n"
        ));
    }
    out
}

/// Two lines: a comment with the description and return shape, then the call
/// signature.
fn render_tool(tool: &ToolSchema) -> String {
    let description = tool
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let description = description.trim_end_matches('.');

    let returns = match &tool.output_schema {
        Some(schema) => {
            let props = sorted_properties(schema);
            if props.is_empty() {
                "table".to_string()
            } else {
                let fields: Vec<String> = props
                    .iter()
                    .map(|(name, prop, _)| match return_type(prop) {
                        Some(ty) => format!("{name}: {ty}"),
                        None => name.to_string(),
                    })
                    .collect();
                format!("{{ {} }}", fields.join(", "))
            }
        }
        None => "table".to_string(),
    };

    let mut comment = if description.is_empty() {
        format!("-- Returns {returns}.")
    } else {
        format!("-- {description}. Returns {returns}.")
    };
    if let Some(missing) = fixed_actor_note(&tool.input_schema) {
        comment.push_str(&format!(
            " There is no `{missing}` argument: the account this acts as is \
             fixed by the gateway's credential."
        ));
    }

    let args: Vec<String> = sorted_properties(&tool.input_schema)
        .iter()
        .map(|(name, prop, required)| {
            let opt = if *required { "" } else { "?" };
            match argument_type(prop) {
                Some(ty) => format!("{name}{opt}: {ty}"),
                None => format!("{name}{opt}"),
            }
        })
        .collect();
    let signature = if args.is_empty() {
        format!("{}{{}} -> table", tool.qualified_name())
    } else {
        format!(
            "{}{{ {} }} -> table",
            tool.qualified_name(),
            args.join(", ")
        )
    };

    format!("{comment}\n{signature}\n")
}

/// Destination arguments and the source argument that would pair with each.
const DIRECTED_PAIRS: [(&str, &str); 4] = [
    ("to", "from"),
    ("to_address", "from_address"),
    ("recipient", "sender"),
    ("destination", "source"),
];

/// The source argument a schema names a destination for but does not itself
/// take, if any. A tool with a `to` and no `from` moves something one way only,
/// out of whatever account the gateway's credential is for: a model that is not
/// told this has to guess, and in the 16 September 2026 cold test every attempt
/// did. Sorted pairs, first match, so the text stays canonical.
fn fixed_actor_note(input_schema: &Value) -> Option<&'static str> {
    let names: Vec<&str> = sorted_properties(input_schema)
        .iter()
        .map(|(name, _, _)| *name)
        .collect();
    DIRECTED_PAIRS
        .iter()
        .find(|(to, from)| names.contains(to) && !names.contains(from))
        .map(|(_, from)| *from)
}

/// `(name, schema, required)` for each property of an object schema, sorted by
/// name.
fn sorted_properties(schema: &Value) -> Vec<(&str, &Value, bool)> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut props: Vec<(&str, &Value, bool)> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| {
            p.iter()
                .map(|(k, v)| (k.as_str(), v, required.contains(&k.as_str())))
                .collect()
        })
        .unwrap_or_default();
    props.sort_by(|a, b| a.0.cmp(b.0));
    props
}

fn argument_type(prop: &Value) -> Option<&'static str> {
    match prop.get("type")?.as_str()? {
        "integer" | "number" => Some("integer"),
        other => simple_type(other),
    }
}

fn return_type(prop: &Value) -> Option<&'static str> {
    match prop.get("type")?.as_str()? {
        "integer" => Some("integer"),
        "number" => Some("integer|string(decimal)"),
        other => simple_type(other),
    }
}

fn simple_type(ty: &str) -> Option<&'static str> {
    match ty {
        "boolean" => Some("boolean"),
        "string" => Some("string"),
        "array" => Some("{...}"),
        "object" => Some("table"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(server: &str, name: &str, input: Value, output: Option<Value>) -> ToolSchema {
        ToolSchema {
            server: server.into(),
            name: name.into(),
            description: format!("Tool {name}"),
            input_schema: input,
            output_schema: output,
        }
    }

    fn tools() -> Vec<ToolSchema> {
        vec![
            schema(
                "wallet",
                "transfer",
                json!({
                    "type": "object",
                    "properties": {
                        "to": { "type": "string" },
                        "amount": { "type": "integer" },
                        "memo": { "type": "string" }
                    },
                    "required": ["amount", "to"]
                }),
                None,
            ),
            schema(
                "market",
                "get_price",
                json!({
                    "type": "object",
                    "properties": { "pair": { "type": "string" } },
                    "required": ["pair"]
                }),
                Some(json!({
                    "type": "object",
                    "properties": { "price": { "type": "number" } }
                })),
            ),
            schema("wallet", "balance", json!({ "type": "object" }), None),
        ]
    }

    #[test]
    fn build_is_identical_for_shuffled_input() {
        let a = build(&tools());
        let mut reversed = tools();
        reversed.reverse();
        let mut rotated = tools();
        rotated.rotate_left(1);
        assert_eq!(a, build(&tools()));
        assert_eq!(a, build(&reversed));
        assert_eq!(a, build(&rotated));
    }

    #[test]
    fn hash_is_sha256_of_text() {
        let d = build(&tools());
        let expected: [u8; 32] = Sha256::digest(d.text.as_bytes()).into();
        assert_eq!(d.hash, expected);
    }

    #[test]
    fn hash_changes_when_a_property_changes() {
        let base = build(&tools());
        let mut changed = tools();
        changed[0].input_schema["properties"]["amount"]["type"] = json!("string");
        assert_ne!(base.hash, build(&changed).hash);
    }

    #[test]
    fn hash_changes_when_a_tool_is_removed() {
        let base = build(&tools());
        let mut fewer = tools();
        fewer.pop();
        assert_ne!(base.hash, build(&fewer).hash);
    }

    #[test]
    fn sections_appear_in_order() {
        let text = build(&tools()).text;
        let rules = text.find("# Lua dialect").unwrap();
        let api = text.find("# Tool API").unwrap();
        let examples = text.find("# Examples").unwrap();
        assert_eq!(rules, 0);
        assert!(rules < api && api < examples);
    }

    #[test]
    fn renders_the_spec_place_order_example() {
        let tool = ToolSchema {
            server: "market".into(),
            name: "place_order".into(),
            description: "Place a limit order".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "size": { "type": "integer" },
                    "price": { "type": "string" },
                    "pair": { "type": "string" }
                },
                "required": ["pair", "size", "price"]
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": { "status": {}, "order_id": {} }
            })),
        };
        // The spec lists the arguments as `pair, size, price`; properties are
        // rendered sorted by name so the text is canonical.
        assert_eq!(
            render_tool(&tool),
            "-- Place a limit order. Returns { order_id, status }.\n\
             market.place_order{ pair: string, price: string, size: integer } -> table\n"
        );
    }

    #[test]
    fn a_schema_with_a_destination_and_no_source_says_so() {
        let tool = schema(
            "wallet",
            "transfer",
            json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string" },
                    "amount": { "type": "integer" }
                },
                "required": ["to", "amount"]
            }),
            None,
        );
        assert!(
            render_tool(&tool).contains(
                "There is no `from` argument: the account this acts as is fixed by the \
                 gateway's credential."
            ),
            "{}",
            render_tool(&tool)
        );
    }

    #[test]
    fn a_schema_with_both_ends_or_neither_says_nothing() {
        let both = schema(
            "ledger",
            "move",
            json!({
                "type": "object",
                "properties": { "to": { "type": "string" }, "from": { "type": "string" } },
                "required": ["to", "from"]
            }),
            None,
        );
        let neither = schema(
            "market",
            "get_price",
            json!({
                "type": "object",
                "properties": { "pair": { "type": "string" } },
                "required": ["pair"]
            }),
            None,
        );
        assert!(!render_tool(&both).contains("There is no"));
        assert!(!render_tool(&neither).contains("There is no"));
    }

    #[test]
    fn renders_optional_and_typed_properties() {
        let rendered: Vec<String> = {
            let mut t = tools();
            t.sort_by_key(|t| t.qualified_name());
            t.iter().map(render_tool).collect()
        };
        assert_eq!(
            rendered,
            vec![
                "-- Tool get_price. Returns { price: integer|string(decimal) }.\n\
                 market.get_price{ pair: string } -> table\n",
                "-- Tool balance. Returns table.\n\
                 wallet.balance{} -> table\n",
                "-- Tool transfer. Returns table. There is no `from` argument: the account \
                 this acts as is fixed by the gateway's credential.\n\
                 wallet.transfer{ amount: integer, memo?: string, to: string } -> table\n",
            ]
        );
    }

    #[test]
    fn argument_types_map_from_json_schema() {
        let tool = schema(
            "s",
            "t",
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "boolean" },
                    "c": { "type": "array" },
                    "d": { "type": "object" }
                },
                "required": ["a", "b", "c", "d"]
            }),
            None,
        );
        assert!(
            render_tool(&tool)
                .contains("s.t{ a: integer, b: boolean, c: {...}, d: table } -> table")
        );
    }

    #[test]
    fn prelude_declares_servers_then_tools_sorted() {
        assert_eq!(
            build(&tools()).prelude,
            "local market = {}\n\
             local wallet = {}\n\
             market.get_price = function(args) return tool.call(\"market.get_price\", args) end\n\
             wallet.balance = function(args) return tool.call(\"wallet.balance\", args) end\n\
             wallet.transfer = function(args) return tool.call(\"wallet.transfer\", args) end\n"
        );
    }

    #[test]
    fn description_lists_only_the_tools_it_is_given() {
        let mut allowed = tools();
        allowed.retain(|t| t.name != "transfer");
        let d = build(&allowed);
        assert!(!d.text.contains("wallet.transfer"));
        assert!(!d.prelude.contains("wallet.transfer"));
    }
}
