//! The execute engine: lint, compile, run, sign and store one program.

// Phase 3 stub

use serde::{Deserialize, Serialize};

use crate::config::GatewayConfig;
use crate::description::ToolDescription;
use crate::dialect::LintError;
use crate::trace::RunStatus;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub program: String,
    pub session: Option<String>,
    pub request: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteResponse {
    pub result: Option<serde_json::Value>,
    pub trace_id: String,
    pub status: RunStatus,
}

pub struct Engine {
    _private: (),
}

impl Engine {
    pub async fn new(_config: GatewayConfig) -> anyhow::Result<Self> {
        todo!("Phase 3")
    }

    pub fn description_for(&self, _principal: &str) -> ToolDescription {
        todo!("Phase 3")
    }

    pub fn check(&self, _principal: &str, _program: &str) -> Result<(), LintError> {
        todo!("Phase 3")
    }

    pub async fn execute(
        &self,
        _principal: &str,
        _req: ExecuteRequest,
    ) -> Result<ExecuteResponse, LintError> {
        todo!("Phase 3")
    }
}
