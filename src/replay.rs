//! Offline replay of a stored trace.

// Phase 4b stub

use serde::{Deserialize, Serialize};

use crate::config::GatewayConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub trace_id: String,
    pub matched: bool,
    pub mismatches: Vec<String>,
}

pub fn replay(_config: &GatewayConfig, _trace_id: &str) -> anyhow::Result<ReplayReport> {
    todo!("Phase 4b")
}
