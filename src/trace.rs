//! The signed per-run trace: a header, one entry per tool call, and a footer.

// Phase 2d stub (types complete now)

use serde::{Deserialize, Serialize};

use crate::config::VmSettings;

/// Which provider produced a response's attestation blob. Bind-only: nothing
/// here verifies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Provenance {
    Unsigned,
    Signed {
        by: String,
        sig: String,
    },
    Onchain {
        chain: String,
        block: u64,
        reference: String,
    },
    Notarized {
        scheme: String,
        reference: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallDecision {
    Allowed,
    RejectedBySchema { reason: String },
    DeniedByPolicy { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceHeader {
    pub trace_id: String,
    pub session: Option<String>,
    pub principal: String,
    /// Lowercase hex.
    pub program_hash: String,
    /// Lowercase hex.
    pub policy_hash: String,
    /// Lowercase hex.
    pub description_hash: String,
    pub vm_version: String,
    pub vm_config: VmSettings,
    pub request: Option<String>,
}

/// No `PartialEq`: `proveno::ToolCallRecord` does not implement it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub record: proveno::ToolCallRecord,
    pub decision: CallDecision,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Error { kind: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceFooter {
    /// Canonical JSON of the return value, `None` on failure.
    pub output: Option<String>,
    pub status: RunStatus,
    pub gas_used: u64,
    pub memory_used: u64,
    /// Hex; empty until signed.
    pub signature: String,
}

/// No `PartialEq`: `TraceEntry` does not implement it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub header: TraceHeader,
    pub entries: Vec<TraceEntry>,
    pub footer: TraceFooter,
}

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("{0}")]
    Invalid(String),
}

impl Trace {
    pub fn sign(&mut self, _key: &ed25519_dalek::SigningKey) {
        todo!("Phase 2d")
    }

    pub fn verify(&self, _key: &ed25519_dalek::VerifyingKey) -> Result<(), TraceError> {
        todo!("Phase 2d")
    }
}

pub fn signing_key_from_hex(_hex: &str) -> Result<ed25519_dalek::SigningKey, TraceError> {
    todo!("Phase 2d")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_round_trips_through_json() {
        let trace = Trace {
            header: TraceHeader {
                trace_id: "0190".into(),
                session: Some("s1".into()),
                principal: "demo-agent".into(),
                program_hash: "00".repeat(32),
                policy_hash: "11".repeat(32),
                description_hash: "22".repeat(32),
                vm_version: crate::VM_VERSION.into(),
                vm_config: VmSettings::default(),
                request: None,
            },
            entries: vec![TraceEntry {
                record: proveno::ToolCallRecord {
                    seq: 0,
                    tool_name: "wallet.transfer".into(),
                    args_canonical: br#"{"amount":60}"#.to_vec(),
                    args_bytes: 13,
                    response_hash: String::new(),
                    response_bytes: 0,
                    response_canonical: Vec::new(),
                    error_message: "amount exceeds 50".into(),
                    attestation: Vec::new(),
                    gas_charged: 0,
                    status: proveno::ToolCallStatus::Error,
                },
                decision: CallDecision::DeniedByPolicy {
                    reason: "amount exceeds 50".into(),
                },
                provenance: Provenance::Unsigned,
            }],
            footer: TraceFooter {
                output: None,
                status: RunStatus::Error {
                    kind: "runtime".into(),
                    message: "amount exceeds 50".into(),
                },
                gas_used: 1234,
                memory_used: 5678,
                signature: String::new(),
            },
        };

        let json = serde_json::to_value(&trace).unwrap();
        assert_eq!(json["entries"][0]["provenance"]["type"], "unsigned");
        assert_eq!(json["entries"][0]["decision"]["type"], "denied_by_policy");

        let back: Trace = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.header, trace.header);
        assert_eq!(back.footer, trace.footer);
        assert_eq!(serde_json::to_value(&back).unwrap(), json);
    }
}
