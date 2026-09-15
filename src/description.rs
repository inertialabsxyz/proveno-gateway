//! The generated tool description and the Lua prelude that goes with it.

// Phase 2c stub

use serde::{Deserialize, Serialize};

use crate::downstream::ToolSchema;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescription {
    pub text: String,
    pub hash: [u8; 32],
    pub prelude: String,
}

pub fn build(_allowed: &[ToolSchema]) -> ToolDescription {
    todo!("Phase 2c")
}
