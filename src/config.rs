//! Gateway configuration: the TOML file described in spec section 3.6.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    #[serde(default)]
    pub vm: VmSettings,
    #[serde(default)]
    pub downstream: Vec<DownstreamConfig>,
    pub policy: PolicyRef,
    pub store: StoreConfig,
    #[serde(default)]
    pub principals: BTreeMap<String, PrincipalConfig>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: String,
    pub signing_key: Secret,
}

/// Mirror of `proveno::VmConfig`, which has no serde derive. It is recorded in
/// the trace header and replay runs with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VmSettings {
    pub gas_limit: u64,
    pub memory_limit_bytes: u64,
    pub max_call_depth: usize,
    pub max_tool_calls: usize,
    pub max_tool_bytes_in: usize,
    pub max_tool_bytes_out: usize,
    pub max_output_bytes: usize,
}

impl Default for VmSettings {
    fn default() -> Self {
        let d = proveno::VmConfig::default();
        VmSettings {
            gas_limit: d.gas_limit,
            memory_limit_bytes: d.memory_limit_bytes,
            max_call_depth: d.max_call_depth,
            max_tool_calls: d.max_tool_calls,
            max_tool_bytes_in: d.max_tool_bytes_in,
            max_tool_bytes_out: d.max_tool_bytes_out,
            max_output_bytes: d.max_output_bytes,
        }
    }
}

impl From<&VmSettings> for proveno::VmConfig {
    fn from(s: &VmSettings) -> Self {
        proveno::VmConfig {
            gas_limit: s.gas_limit,
            memory_limit_bytes: s.memory_limit_bytes,
            max_call_depth: s.max_call_depth,
            max_tool_calls: s.max_tool_calls,
            max_tool_bytes_in: s.max_tool_bytes_in,
            max_tool_bytes_out: s.max_tool_bytes_out,
            max_output_bytes: s.max_output_bytes,
            record_trace: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawDownstream")]
pub struct DownstreamConfig {
    pub name: String,
    pub transport: Transport,
    pub credential: Option<Secret>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Transport {
    Stdio { command: String },
    Http { url: String },
}

/// The flat TOML shape: `transport = "stdio" | "http"` with `command` or `url`
/// as siblings.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDownstream {
    name: String,
    transport: String,
    command: Option<String>,
    url: Option<String>,
    credential: Option<Secret>,
}

impl TryFrom<RawDownstream> for DownstreamConfig {
    type Error = String;

    fn try_from(raw: RawDownstream) -> Result<Self, String> {
        let name = &raw.name;
        let transport = match (raw.transport.as_str(), raw.command, raw.url) {
            ("stdio", Some(command), None) => Transport::Stdio { command },
            ("http", None, Some(url)) => Transport::Http { url },
            ("stdio", _, Some(_)) => {
                return Err(format!(
                    "downstream `{name}`: stdio transport takes `command`, not `url`"
                ));
            }
            ("stdio", None, None) => {
                return Err(format!(
                    "downstream `{name}`: stdio transport requires `command`"
                ));
            }
            ("http", Some(_), _) => {
                return Err(format!(
                    "downstream `{name}`: http transport takes `url`, not `command`"
                ));
            }
            ("http", None, None) => {
                return Err(format!(
                    "downstream `{name}`: http transport requires `url`"
                ));
            }
            (other, _, _) => {
                return Err(format!(
                    "downstream `{name}`: unknown transport `{other}`, expected `stdio` or `http`"
                ));
            }
        };
        Ok(DownstreamConfig {
            name: raw.name,
            transport,
            credential: raw.credential,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRef {
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalConfig {
    pub token: Secret,
}

/// A reference to a secret, never the secret itself. Only `env:NAME` is
/// accepted, so the config file never holds a credential.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct Secret(String);

impl TryFrom<String> for Secret {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        match s.strip_prefix("env:") {
            Some(name) if !name.is_empty() => Ok(Secret(s)),
            _ => Err(format!(
                "secret `{s}` must have the form `env:NAME`; no other form is accepted"
            )),
        }
    }
}

impl Secret {
    /// The variable name in `env:NAME`. It names the secret, it is not the
    /// secret.
    pub fn env_name(&self) -> &str {
        &self.0["env:".len()..]
    }

    /// Reads the named environment variable.
    pub fn resolve(&self) -> Result<String, ConfigError> {
        let name = self.env_name();
        std::env::var(name)
            .map_err(|_| ConfigError::Invalid(format!("environment variable `{name}` is not set")))
    }
}

pub fn load(path: &Path) -> Result<GatewayConfig, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError::Invalid(format!("{}: {e}", path.display())))?;
    let base = path.parent().unwrap_or(Path::new(""));
    parse(&text, base).map_err(|e| ConfigError::Invalid(format!("{}: {e}", path.display())))
}

fn parse(text: &str, base: &Path) -> Result<GatewayConfig, ConfigError> {
    let mut config: GatewayConfig =
        toml::from_str(text).map_err(|e| ConfigError::Invalid(e.to_string()))?;

    let mut names = BTreeSet::new();
    for d in &config.downstream {
        if d.name.contains('.') {
            return Err(ConfigError::Invalid(format!(
                "downstream `{}`: name must not contain `.`",
                d.name
            )));
        }
        if !names.insert(d.name.as_str()) {
            return Err(ConfigError::Invalid(format!(
                "downstream `{}`: name is used more than once",
                d.name
            )));
        }
    }

    config.policy.file = base.join(&config.policy.file);
    config.store.dir = base.join(&config.store.dir);
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[server]
listen = "127.0.0.1:7777"
signing_key = "env:PROVENO_SIGNING_KEY"

[vm]
gas_limit = 2_000_000
memory_limit_bytes = 16_777_216
max_tool_calls = 64

[[downstream]]
name = "wallet"
transport = "stdio"           # or http
command = "agentkit-mcp"
credential = "env:WALLET_API_KEY"

[[downstream]]
name = "market"
transport = "http"
url = "http://localhost:8080/mcp"
credential = "env:MARKET_KEY"

[policy]
file = "policy.toml"

[store]
dir = "traces"

# Section 8, identity of the principal: a static bearer token per agent.
[principals.demo-agent]
token = "env:DEMO_AGENT_TOKEN"
"#;

    const MINIMAL: &str = r#"
[server]
listen = "127.0.0.1:7777"
signing_key = "env:PROVENO_SIGNING_KEY"

[policy]
file = "policy.toml"

[store]
dir = "traces"
"#;

    fn err(text: &str) -> String {
        parse(text, Path::new("")).unwrap_err().to_string()
    }

    fn with_downstream(block: &str) -> String {
        format!("{MINIMAL}\n[[downstream]]\n{block}")
    }

    #[test]
    fn spec_example_parses() {
        let config = parse(EXAMPLE, Path::new("")).unwrap();
        assert_eq!(config.server.listen, "127.0.0.1:7777");
        assert_eq!(config.vm.gas_limit, 2_000_000);
        assert_eq!(config.vm.max_tool_calls, 64);
        assert_eq!(
            config.downstream,
            vec![
                DownstreamConfig {
                    name: "wallet".into(),
                    transport: Transport::Stdio {
                        command: "agentkit-mcp".into()
                    },
                    credential: Some(Secret("env:WALLET_API_KEY".into())),
                },
                DownstreamConfig {
                    name: "market".into(),
                    transport: Transport::Http {
                        url: "http://localhost:8080/mcp".into()
                    },
                    credential: Some(Secret("env:MARKET_KEY".into())),
                },
            ]
        );
        assert_eq!(config.policy.file, PathBuf::from("policy.toml"));
        assert_eq!(config.store.dir, PathBuf::from("traces"));
        assert_eq!(
            config.principals["demo-agent"].token,
            Secret("env:DEMO_AGENT_TOKEN".into())
        );
    }

    #[test]
    fn non_env_secret_is_rejected() {
        let text = MINIMAL.replace("env:PROVENO_SIGNING_KEY", "deadbeef");
        assert!(err(&text).contains("env:NAME"));
        let text = MINIMAL.replace("env:PROVENO_SIGNING_KEY", "env:");
        assert!(err(&text).contains("env:NAME"));
    }

    #[test]
    fn http_without_url_is_rejected() {
        let e = err(&with_downstream(
            "name = \"market\"\ntransport = \"http\"\n",
        ));
        assert!(e.contains("downstream `market`"), "{e}");
        assert!(e.contains("requires `url`"), "{e}");
    }

    #[test]
    fn mismatched_transport_field_is_rejected() {
        let e = err(&with_downstream(
            "name = \"wallet\"\ntransport = \"stdio\"\nurl = \"http://x\"\n",
        ));
        assert!(e.contains("downstream `wallet`"), "{e}");
        let e = err(&with_downstream(
            "name = \"wallet\"\ntransport = \"stdio\"\ncommand = \"x\"\nurl = \"http://x\"\n",
        ));
        assert!(e.contains("downstream `wallet`"), "{e}");
        let e = err(&with_downstream(
            "name = \"market\"\ntransport = \"http\"\ncommand = \"x\"\n",
        ));
        assert!(e.contains("downstream `market`"), "{e}");
    }

    #[test]
    fn omitted_vm_fields_equal_core_defaults() {
        let config = parse(MINIMAL, Path::new("")).unwrap();
        let vm = proveno::VmConfig::from(&config.vm);
        let d = proveno::VmConfig::default();
        assert_eq!(vm.gas_limit, d.gas_limit);
        assert_eq!(vm.memory_limit_bytes, d.memory_limit_bytes);
        assert_eq!(vm.max_call_depth, d.max_call_depth);
        assert_eq!(vm.max_tool_calls, d.max_tool_calls);
        assert_eq!(vm.max_tool_bytes_in, d.max_tool_bytes_in);
        assert_eq!(vm.max_tool_bytes_out, d.max_tool_bytes_out);
        assert_eq!(vm.max_output_bytes, d.max_output_bytes);
        assert!(!vm.record_trace);

        let partial = parse(EXAMPLE, Path::new("")).unwrap();
        assert_eq!(partial.vm.max_call_depth, d.max_call_depth);
        assert_eq!(partial.vm.max_output_bytes, d.max_output_bytes);
    }

    #[test]
    fn downstream_names_are_unique_and_undotted() {
        let one = "name = \"wallet\"\ntransport = \"stdio\"\ncommand = \"x\"\n";
        let e = err(&format!("{}\n[[downstream]]\n{one}", with_downstream(one)));
        assert!(e.contains("more than once"), "{e}");
        let e = err(&with_downstream(
            "name = \"my.wallet\"\ntransport = \"stdio\"\ncommand = \"x\"\n",
        ));
        assert!(e.contains("must not contain `.`"), "{e}");
    }

    #[test]
    fn relative_paths_resolve_against_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.toml");
        std::fs::write(&path, MINIMAL.replace("\"traces\"", "\"/abs/traces\"")).unwrap();
        let config = load(&path).unwrap();
        assert_eq!(config.policy.file, dir.path().join("policy.toml"));
        assert_eq!(config.store.dir, PathBuf::from("/abs/traces"));
    }

    #[test]
    fn secret_env_name_is_the_variable_name() {
        let secret = Secret::try_from("env:WALLET_API_KEY".to_string()).unwrap();
        assert_eq!(secret.env_name(), "WALLET_API_KEY");
    }

    #[test]
    fn secret_resolves_from_environment() {
        let set = Secret::try_from("env:PATH".to_string()).unwrap();
        assert_eq!(set.resolve().unwrap(), std::env::var("PATH").unwrap());
        let unset = Secret::try_from("env:PROVENO_GATEWAY_TEST_UNSET".to_string()).unwrap();
        assert!(
            unset
                .resolve()
                .unwrap_err()
                .to_string()
                .contains("PROVENO_GATEWAY_TEST_UNSET")
        );
    }
}
