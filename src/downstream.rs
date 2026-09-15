//! MCP client connections to the downstream tool servers.

// Phase 2a stub

use serde::{Deserialize, Serialize};

use crate::config::DownstreamConfig;

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
    _private: (),
}

#[derive(Debug, thiserror::Error)]
pub enum DownstreamError {
    #[error("{0}")]
    Connect(String),
}

impl Downstreams {
    pub async fn connect(_configs: &[DownstreamConfig]) -> Result<Self, DownstreamError> {
        todo!("Phase 2a")
    }

    /// Every discovered tool, sorted by qualified name.
    pub fn tools(&self) -> &[ToolSchema] {
        todo!("Phase 2a")
    }

    pub async fn call(
        &self,
        _qualified: &str,
        _args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        todo!("Phase 2a")
    }
}

#[cfg(test)]
mod tests {
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
}
