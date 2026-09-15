//! Validation of tool call arguments against the downstream JSON schema.

use crate::downstream::ToolSchema;

pub fn check_args(tool: &ToolSchema, args: &serde_json::Value) -> Result<(), String> {
    let name = tool.qualified_name();
    // Offline: a downstream schema must never make the gateway fetch a `$ref`.
    let validator = jsonschema::options()
        .offline()
        .build(&tool.input_schema)
        .map_err(|e| format!("{name}: invalid input schema: {e}"))?;
    match validator.iter_errors(args).next() {
        None => Ok(()),
        Some(e) => {
            let path = e.instance_path().as_str();
            let path = if path.is_empty() { "/" } else { path };
            Err(format!("{name}: invalid arguments at {path}: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn transfer(input_schema: serde_json::Value) -> ToolSchema {
        ToolSchema {
            server: "wallet".into(),
            name: "transfer".into(),
            description: String::new(),
            input_schema,
            output_schema: None,
        }
    }

    fn transfer_schema() -> ToolSchema {
        transfer(json!({
            "type": "object",
            "properties": {
                "to": { "type": "string" },
                "amount": { "type": "integer" }
            },
            "required": ["to", "amount"]
        }))
    }

    #[test]
    fn valid_call_passes() {
        let args = json!({ "to": "0x1", "amount": 20 });
        assert_eq!(check_args(&transfer_schema(), &args), Ok(()));
    }

    #[test]
    fn missing_required_property_fails() {
        let err = check_args(&transfer_schema(), &json!({ "to": "0x1" })).unwrap_err();
        assert!(
            err.starts_with("wallet.transfer: invalid arguments at /:"),
            "{err}"
        );
        assert!(err.contains("amount"), "{err}");
    }

    #[test]
    fn wrong_type_names_the_failing_path() {
        let args = json!({ "to": "0x1", "amount": "twenty" });
        let err = check_args(&transfer_schema(), &args).unwrap_err();
        assert!(
            err.starts_with("wallet.transfer: invalid arguments at /amount:"),
            "{err}"
        );
    }

    #[test]
    fn invalid_schema_is_an_error() {
        let err = check_args(&transfer(json!({ "type": 12 })), &json!({})).unwrap_err();
        assert!(
            err.starts_with("wallet.transfer: invalid input schema"),
            "{err}"
        );
    }

    #[test]
    fn external_ref_is_not_fetched() {
        let schema = transfer(json!({ "$ref": "https://example.com/schema.json" }));
        let err = check_args(&schema, &json!({})).unwrap_err();
        assert!(
            err.starts_with("wallet.transfer: invalid input schema"),
            "{err}"
        );
    }
}
