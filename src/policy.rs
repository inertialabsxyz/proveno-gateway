//! The static policy rules file: allow-lists and per-argument constraints.

// Phase 2b stub

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

/// Spec section 8, session state: always empty in the prototype.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {}

pub struct Policy {
    _private: (),
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Invalid(String),
}

impl Policy {
    pub fn load(_path: &Path) -> Result<Self, PolicyError> {
        todo!("Phase 2b")
    }

    pub fn policy_hash(&self) -> [u8; 32] {
        todo!("Phase 2b")
    }

    pub fn allowed_tools(&self, _principal: &str) -> BTreeSet<String> {
        todo!("Phase 2b")
    }

    pub fn check(
        &self,
        _principal: &str,
        _tool: &str,
        _args: &serde_json::Value,
        _session: &SessionState,
    ) -> Decision {
        todo!("Phase 2b")
    }
}
