//! Validation of tool call arguments against the downstream JSON schema.

// Phase 2b stub

use crate::downstream::ToolSchema;

pub fn check_args(_tool: &ToolSchema, _args: &serde_json::Value) -> Result<(), String> {
    todo!("Phase 2b")
}
