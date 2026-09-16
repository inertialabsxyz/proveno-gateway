//! The signed per-run trace: a header, one entry per tool call, and a footer.
//!
//! Serialization is `serde_json` of [`Trace`], struct fields in declaration
//! order. The signature is Ed25519 over the SHA-256 of that serialization with
//! `footer.signature` empty, stored as lowercase hex (spec section 3.4).

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// Lowercase hex of the gateway's canonical rules file. This is not a
    /// proof's `policy_hash`, which commits to proveno-zk's `OraclePolicy`:
    /// they are two documents with two commitments (spec section 6).
    pub gateway_policy_hash: String,
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
    #[error("signing key must be 64 hex characters (a 32-byte seed)")]
    InvalidSigningKey,
    #[error("trace signature is malformed")]
    MalformedSignature,
    #[error("trace signature does not match")]
    SignatureMismatch,
}

impl Trace {
    pub fn sign(&mut self, key: &SigningKey) {
        self.footer.signature.clear();
        let signature = key.sign(&self.digest());
        self.footer.signature = hex::encode(signature.to_bytes());
    }

    pub fn verify(&self, key: &VerifyingKey) -> Result<(), TraceError> {
        let mut bytes = [0u8; ed25519_dalek::SIGNATURE_LENGTH];
        hex::decode_to_slice(&self.footer.signature, &mut bytes)
            .map_err(|_| TraceError::MalformedSignature)?;
        let signature = Signature::from_bytes(&bytes);

        let mut unsigned = self.clone();
        unsigned.footer.signature.clear();
        key.verify_strict(&unsigned.digest(), &signature)
            .map_err(|_| TraceError::SignatureMismatch)
    }

    /// SHA-256 of the JSON serialization, as it stands.
    fn digest(&self) -> [u8; 32] {
        // Every field is a string, integer, sequence or struct, so serializing
        // to a buffer cannot fail.
        let json = serde_json::to_vec(self).expect("trace serializes to JSON");
        Sha256::digest(json).into()
    }
}

/// Parse a hex-encoded 32-byte Ed25519 seed. The error never echoes the input.
pub fn signing_key_from_hex(hex: &str) -> Result<SigningKey, TraceError> {
    let mut seed = [0u8; ed25519_dalek::SECRET_KEY_LENGTH];
    hex::decode_to_slice(hex, &mut seed).map_err(|_| TraceError::InvalidSigningKey)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// A fixed trace with one successful call and one policy denial, shared by the
/// trace and store tests.
#[cfg(test)]
pub(crate) fn sample_trace() -> Trace {
    Trace {
        header: TraceHeader {
            trace_id: "0190".into(),
            session: Some("s1".into()),
            principal: "demo-agent".into(),
            program_hash: "00".repeat(32),
            gateway_policy_hash: "11".repeat(32),
            description_hash: "22".repeat(32),
            vm_version: "0.2.0".into(),
            vm_config: VmSettings {
                gas_limit: 2_000_000,
                memory_limit_bytes: 16_777_216,
                max_call_depth: 64,
                max_tool_calls: 64,
                max_tool_bytes_in: 4096,
                max_tool_bytes_out: 4096,
                max_output_bytes: 4096,
            },
            request: None,
        },
        entries: vec![
            TraceEntry {
                record: proveno::ToolCallRecord {
                    seq: 0,
                    tool_name: "market.get_price".into(),
                    args_canonical: br#"{"a":"E"}"#.to_vec(),
                    args_bytes: 9,
                    response_hash: "33".repeat(32),
                    response_bytes: 7,
                    response_canonical: br#"{"p":7}"#.to_vec(),
                    error_message: String::new(),
                    attestation: vec![1, 2],
                    gas_charged: 10,
                    status: proveno::ToolCallStatus::Ok,
                },
                decision: CallDecision::Allowed,
                provenance: Provenance::Unsigned,
            },
            TraceEntry {
                record: proveno::ToolCallRecord {
                    seq: 1,
                    tool_name: "wallet.transfer".into(),
                    args_canonical: br#"{"n":60}"#.to_vec(),
                    args_bytes: 8,
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
            },
        ],
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
    }
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
                gateway_policy_hash: "11".repeat(32),
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

    /// The trace format is signed, so it must not drift. A change here breaks
    /// every stored signature.
    const PINNED_JSON: &str = concat!(
        r#"{"header":{"trace_id":"0190","session":"s1","principal":"demo-agent","#,
        r#""program_hash":"0000000000000000000000000000000000000000000000000000000000000000","#,
        r#""gateway_policy_hash":"1111111111111111111111111111111111111111111111111111111111111111","#,
        r#""description_hash":"2222222222222222222222222222222222222222222222222222222222222222","#,
        r#""vm_version":"0.2.0","#,
        r#""vm_config":{"gas_limit":2000000,"memory_limit_bytes":16777216,"max_call_depth":64,"#,
        r#""max_tool_calls":64,"max_tool_bytes_in":4096,"max_tool_bytes_out":4096,"#,
        r#""max_output_bytes":4096},"request":null},"#,
        r#""entries":[{"record":{"seq":0,"tool_name":"market.get_price","#,
        r#""args_canonical":"{\"a\":\"E\"}","args_bytes":9,"#,
        r#""response_hash":"3333333333333333333333333333333333333333333333333333333333333333","#,
        r#""response_bytes":7,"response_canonical":"{\"p\":7}","#,
        r#""error_message":"","attestation":"0102","gas_charged":10,"status":"Ok"},"#,
        r#""decision":{"type":"allowed"},"provenance":{"type":"unsigned"}},"#,
        r#"{"record":{"seq":1,"tool_name":"wallet.transfer","#,
        r#""args_canonical":"{\"n\":60}","args_bytes":8,"#,
        r#""response_hash":"","response_bytes":0,"response_canonical":"","#,
        r#""error_message":"amount exceeds 50","attestation":"","gas_charged":0,"status":"Error"},"#,
        r#""decision":{"type":"denied_by_policy","reason":"amount exceeds 50"},"#,
        r#""provenance":{"type":"unsigned"}}],"#,
        r#""footer":{"output":null,"#,
        r#""status":{"type":"error","kind":"runtime","message":"amount exceeds 50"},"#,
        r#""gas_used":1234,"memory_used":5678,"signature":""}}"#,
    );

    /// Ed25519 is deterministic, so a fixed key and trace give a fixed signature.
    const PINNED_SIGNATURE: &str = concat!(
        "0af5e68afa230d3443bec345671ea46a455ff8d3429ed85f77789c14dae5b84c",
        "1b02eda6484432e5130c356a30ba3f9c90d0125b77d3df559fc8c88ad4a0eb0a",
    );

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    #[test]
    fn json_serialization_is_pinned() {
        let json = serde_json::to_string(&sample_trace()).unwrap();
        assert_eq!(json, PINNED_JSON);
    }

    #[test]
    fn signature_is_pinned() {
        let mut trace = sample_trace();
        trace.sign(&key(7));
        assert_eq!(trace.footer.signature, PINNED_SIGNATURE);
    }

    #[test]
    fn signature_covers_sha256_of_json_with_empty_signature() {
        let mut trace = sample_trace();
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&trace).unwrap()).into();
        trace.sign(&key(7));

        let expected = key(7).sign(&digest);
        assert_eq!(trace.footer.signature, hex::encode(expected.to_bytes()));
        assert_eq!(trace.footer.signature.len(), 128);
        assert_eq!(
            trace.footer.signature,
            trace.footer.signature.to_lowercase()
        );
    }

    #[test]
    fn sign_then_verify_succeeds() {
        let mut trace = sample_trace();
        trace.sign(&key(7));
        trace.verify(&key(7).verifying_key()).unwrap();
    }

    #[test]
    fn resigning_a_signed_trace_gives_the_same_signature() {
        let mut trace = sample_trace();
        trace.sign(&key(7));
        let first = trace.footer.signature.clone();
        trace.sign(&key(7));
        assert_eq!(trace.footer.signature, first);
    }

    fn assert_tamper_detected(tamper: impl FnOnce(&mut Trace)) {
        let mut trace = sample_trace();
        trace.sign(&key(7));
        tamper(&mut trace);
        assert!(matches!(
            trace.verify(&key(7).verifying_key()),
            Err(TraceError::SignatureMismatch)
        ));
    }

    #[test]
    fn verify_fails_after_changing_output() {
        assert_tamper_detected(|t| t.footer.output = Some("1".into()));
    }

    #[test]
    fn verify_fails_after_changing_response_canonical() {
        assert_tamper_detected(|t| t.entries[0].record.response_canonical = br#"{"p":8}"#.to_vec());
    }

    #[test]
    fn verify_fails_after_changing_gas_limit() {
        assert_tamper_detected(|t| t.header.vm_config.gas_limit += 1);
    }

    #[test]
    fn verify_fails_with_a_different_key() {
        let mut trace = sample_trace();
        trace.sign(&key(7));
        assert!(matches!(
            trace.verify(&key(8).verifying_key()),
            Err(TraceError::SignatureMismatch)
        ));
    }

    #[test]
    fn verify_reports_a_malformed_signature() {
        for bad in [
            "",
            "zz",
            &"ab".repeat(63),
            &"ab".repeat(65),
            &"g".repeat(128),
        ] {
            let mut trace = sample_trace();
            trace.footer.signature = bad.to_string();
            assert!(
                matches!(
                    trace.verify(&key(7).verifying_key()),
                    Err(TraceError::MalformedSignature)
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn signing_key_from_hex_accepts_a_32_byte_seed() {
        let parsed = signing_key_from_hex(&"07".repeat(32)).unwrap();
        assert_eq!(parsed.to_bytes(), key(7).to_bytes());
    }

    #[test]
    fn signing_key_from_hex_rejects_anything_else_without_echoing_it() {
        let secret = "5ecre7";
        for bad in [
            String::new(),
            secret.to_string(),
            format!("{secret}{}", "0".repeat(58)),
            "0".repeat(63),
            "0".repeat(65),
            "0".repeat(66),
            format!("0x{}", "0".repeat(62)),
            format!("{}zz", "0".repeat(62)),
        ] {
            let err = signing_key_from_hex(&bad).unwrap_err();
            assert!(matches!(err, TraceError::InvalidSigningKey), "{bad:?}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(secret), "{shown}");
            if !bad.is_empty() {
                assert!(!shown.contains(&bad), "{shown}");
            }
        }
    }
}
