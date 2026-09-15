//! The static policy rules file: allow-lists and per-argument constraints.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

/// Spec section 8, session state: always empty in the prototype.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {}

/// The parsed `policy.toml`. Every collection is ordered, so its serialization
/// is canonical and `policy_hash` ignores file layout.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Policy {
    principals: BTreeMap<String, BTreeSet<String>>,
    /// Qualified tool name to argument name to inclusive upper bound.
    constraints: BTreeMap<String, BTreeMap<String, i64>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    principals: BTreeMap<String, PrincipalFile>,
    #[serde(default)]
    constraints: BTreeMap<String, BTreeMap<String, i64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalFile {
    allow: Vec<String>,
}

impl Policy {
    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| PolicyError::Invalid(format!("{}: {e}", path.display())))?;
        Self::parse(&text).map_err(|e| PolicyError::Invalid(format!("{}: {e}", path.display())))
    }

    fn parse(text: &str) -> Result<Self, PolicyError> {
        let file: PolicyFile =
            toml::from_str(text).map_err(|e| PolicyError::Invalid(e.to_string()))?;

        let principals = file
            .principals
            .into_iter()
            .map(|(name, p)| (name, p.allow.into_iter().collect()))
            .collect();

        let mut constraints = BTreeMap::new();
        for (tool, bounds) in file.constraints {
            let mut args = BTreeMap::new();
            for (key, max) in bounds {
                let arg = key
                    .strip_suffix("_max")
                    .filter(|arg| !arg.is_empty())
                    .ok_or_else(|| {
                        PolicyError::Invalid(format!(
                            "constraint {tool}.{key}: only <arg>_max bounds are supported"
                        ))
                    })?;
                args.insert(arg.to_string(), max);
            }
            constraints.insert(tool, args);
        }

        Ok(Policy {
            principals,
            constraints,
        })
    }

    pub fn policy_hash(&self) -> [u8; 32] {
        let canonical = serde_json::to_vec(self).expect("policy serializes to JSON");
        Sha256::digest(&canonical).into()
    }

    pub fn allowed_tools(&self, principal: &str) -> BTreeSet<String> {
        self.principals.get(principal).cloned().unwrap_or_default()
    }

    pub fn check(
        &self,
        principal: &str,
        tool: &str,
        args: &serde_json::Value,
        _session: &SessionState,
    ) -> Decision {
        let Some(allow) = self.principals.get(principal) else {
            return deny(format!("unknown principal {principal}"));
        };
        if !allow.contains(tool) {
            return deny(format!("{tool} is not allowed for principal {principal}"));
        }
        for (arg, max) in self.constraints.get(tool).into_iter().flatten() {
            let Some(value) = args.get(arg) else {
                return deny(format!("{tool}: {arg} is required by {arg}_max"));
            };
            let exceeds = match (value.as_i64(), value.as_u64()) {
                (Some(n), _) => n > *max,
                // A u64 beyond i64::MAX is above any i64 bound.
                (None, Some(_)) => true,
                (None, None) => {
                    return deny(format!("{tool}: {arg} {value} is not an integer"));
                }
            };
            if exceeds {
                return deny(format!("{tool}: {arg} {value} exceeds {arg}_max {max}"));
            }
        }
        Decision::Allow
    }
}

fn deny(reason: String) -> Decision {
    Decision::Deny { reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXAMPLE: &str = r#"
[principals.demo-agent]
allow = ["wallet.get_balance", "market.get_price", "wallet.transfer"]

[constraints."wallet.transfer"]
amount_max = 50
"#;

    fn example() -> Policy {
        Policy::parse(EXAMPLE).unwrap()
    }

    fn check(policy: &Policy, principal: &str, tool: &str, args: serde_json::Value) -> Decision {
        policy.check(principal, tool, &args, &SessionState {})
    }

    fn reason(decision: Decision) -> String {
        match decision {
            Decision::Deny { reason } => reason,
            Decision::Allow => panic!("expected a denial"),
        }
    }

    #[test]
    fn spec_example_loads_from_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), EXAMPLE).unwrap();
        let policy = Policy::load(file.path()).unwrap();
        assert_eq!(
            policy.allowed_tools("demo-agent"),
            BTreeSet::from([
                "market.get_price".to_string(),
                "wallet.get_balance".to_string(),
                "wallet.transfer".to_string(),
            ])
        );
        assert_eq!(policy, example());
    }

    #[test]
    fn unknown_principal_has_no_tools() {
        assert!(example().allowed_tools("nobody").is_empty());
    }

    #[test]
    fn amount_within_max_is_allowed() {
        let decision = check(
            &example(),
            "demo-agent",
            "wallet.transfer",
            json!({ "to": "0x1", "amount": 20 }),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn amount_equal_to_max_is_allowed() {
        let decision = check(
            &example(),
            "demo-agent",
            "wallet.transfer",
            json!({ "amount": 50 }),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn unconstrained_allowed_tool_is_allowed() {
        let decision = check(
            &example(),
            "demo-agent",
            "market.get_price",
            json!({ "symbol": "ETH" }),
        );
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn amount_over_max_is_denied() {
        let decision = check(
            &example(),
            "demo-agent",
            "wallet.transfer",
            json!({ "to": "0x1", "amount": 60 }),
        );
        assert_eq!(
            reason(decision),
            "wallet.transfer: amount 60 exceeds amount_max 50"
        );
    }

    #[test]
    fn disallowed_tool_is_denied() {
        let decision = check(&example(), "demo-agent", "wallet.drain", json!({}));
        assert_eq!(
            reason(decision),
            "wallet.drain is not allowed for principal demo-agent"
        );
    }

    #[test]
    fn unknown_principal_is_denied() {
        let decision = check(&example(), "intruder", "wallet.get_balance", json!({}));
        assert_eq!(reason(decision), "unknown principal intruder");
    }

    #[test]
    fn missing_constrained_argument_is_denied() {
        let decision = check(
            &example(),
            "demo-agent",
            "wallet.transfer",
            json!({ "to": "0x1" }),
        );
        assert_eq!(
            reason(decision),
            "wallet.transfer: amount is required by amount_max"
        );
    }

    #[test]
    fn non_integer_constrained_argument_is_denied() {
        for amount in [json!(20.5), json!("20")] {
            let decision = check(
                &example(),
                "demo-agent",
                "wallet.transfer",
                json!({ "amount": amount }),
            );
            assert!(reason(decision).contains("is not an integer"));
        }
    }

    #[test]
    fn u64_beyond_i64_is_denied() {
        let decision = check(
            &example(),
            "demo-agent",
            "wallet.transfer",
            json!({ "amount": u64::MAX }),
        );
        assert!(reason(decision).contains("exceeds amount_max 50"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Policy::parse("[rules]\nx = 1\n").is_err());
        assert!(Policy::parse("[principals.a]\nallow = []\ndeny = []\n").is_err());
    }

    #[test]
    fn constraint_without_max_suffix_is_rejected() {
        let err = Policy::parse("[constraints.\"wallet.transfer\"]\namount_min = 1\n").unwrap_err();
        assert!(err.to_string().contains("amount_min"), "{err}");
        assert!(Policy::parse("[constraints.\"wallet.transfer\"]\n_max = 1\n").is_err());
    }

    #[test]
    fn hash_ignores_reordering_and_formatting() {
        let reordered = r#"
# the same policy, laid out differently
[constraints."wallet.transfer"]
amount_max   =   50

[principals.demo-agent]
allow = [
    "wallet.transfer",
    "market.get_price",
    "wallet.get_balance",
]
"#;
        assert_eq!(
            example().policy_hash(),
            Policy::parse(reordered).unwrap().policy_hash()
        );
    }

    #[test]
    fn hash_changes_when_amount_max_changes() {
        let tighter = EXAMPLE.replace("amount_max = 50", "amount_max = 10");
        assert_ne!(
            example().policy_hash(),
            Policy::parse(&tighter).unwrap().policy_hash()
        );
    }
}
