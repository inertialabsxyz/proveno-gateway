//! The execute engine: lint, compile, run, sign and store one program.

use std::sync::Arc;

use anyhow::Context;
use ed25519_dalek::SigningKey;
use proveno::compiler::program_hash::compute_program_hash_sha256;
use proveno::host::canonicalize::canonical_serialize;
use proveno::types::value::LuaValue;
use proveno::{Vm, VmConfig, VmError};
use serde::{Deserialize, Serialize};

use crate::config::{GatewayConfig, VmSettings};
use crate::description::{self, ToolDescription};
use crate::dialect::{LintError, compile_program};
use crate::downstream::{Downstreams, ToolSchema};
use crate::host::GatewayHost;
use crate::policy::Policy;
use crate::store::TraceStore;
use crate::trace::{RunStatus, Trace, TraceEntry, TraceFooter, TraceHeader, signing_key_from_hex};

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
    vm: VmSettings,
    policy: Arc<Policy>,
    downstreams: Arc<Downstreams>,
    store: TraceStore,
    signing_key: SigningKey,
}

impl Engine {
    /// Loads everything a run needs, so a missing secret, policy or downstream
    /// fails here rather than on the first request.
    pub async fn new(config: GatewayConfig) -> anyhow::Result<Self> {
        let policy = Policy::load(&config.policy.file)?;
        let downstreams = Downstreams::connect(&config.downstream).await?;
        let store = TraceStore::open(&config.store.dir)
            .with_context(|| format!("store {}", config.store.dir.display()))?;
        let signing_key = signing_key_from_hex(
            &config
                .server
                .signing_key
                .resolve()
                .context("server.signing_key")?,
        )
        .context("server.signing_key")?;
        for (name, principal) in &config.principals {
            principal
                .token
                .resolve()
                .with_context(|| format!("principal `{name}` token"))?;
        }
        let engine = Engine {
            vm: config.vm,
            policy: Arc::new(policy),
            downstreams: Arc::new(downstreams),
            store,
            signing_key,
        };
        // The prelude declares every allowed tool as Lua, so a name Lua cannot
        // declare would otherwise surface as a compile panic on first use.
        for principal in config.principals.keys() {
            for tool in engine.allowed_schemas(principal) {
                check_lua_names(&tool)
                    .map_err(|e| anyhow::anyhow!("principal `{principal}`: {e}"))?;
            }
        }
        Ok(engine)
    }

    pub fn description_for(&self, principal: &str) -> ToolDescription {
        description::build(&self.allowed_schemas(principal))
    }

    pub fn check(&self, principal: &str, program: &str) -> Result<(), LintError> {
        compile_program(&self.description_for(principal).prelude, program).map(|_| ())
    }

    /// Runs one program and stores its signed trace. A lint error is returned
    /// before anything is stored; every run that starts produces a trace,
    /// whether it succeeds or fails.
    pub async fn execute(
        &self,
        principal: &str,
        req: ExecuteRequest,
    ) -> Result<ExecuteResponse, LintError> {
        let allowed = self.allowed_schemas(principal);
        let description = description::build(&allowed);
        let program = compile_program(&description.prelude, &req.program)?;
        let program_hash = hex::encode(compute_program_hash_sha256(&program.prototypes));
        let description_hash = hex::encode(description.hash);

        // The prelude always ends in a newline (or is empty), so this is exactly
        // the source `compile_program` compiled, and it recompiles to
        // `program_hash` on its own.
        let source = format!("{}{}", description.prelude, req.program);
        self.store
            .put_program(&program_hash, &source)
            .expect("trace store: write program");
        self.store
            .put_description(&description_hash, &description.text)
            .expect("trace store: write description");

        let trace_id = uuid::Uuid::now_v7().to_string();
        let host = GatewayHost::new(
            principal.to_string(),
            Arc::clone(&self.downstreams),
            Arc::new(allowed),
            Arc::clone(&self.policy),
            tokio::runtime::Handle::current(),
        );
        let vm_config = VmConfig::from(&self.vm);
        let max_output_bytes = self.vm.max_output_bytes;
        let prelude_lines = description.prelude.matches('\n').count() as u32;

        // The VM is synchronous and the host blocks on the runtime for each tool
        // call, so the run goes on a blocking thread.
        let run = tokio::task::spawn_blocking(move || {
            let mut vm = Vm::new(vm_config, host);
            let (output, status, gas_used, memory_used) = match vm.execute(&program, LuaValue::Nil)
            {
                Ok(out) => match output_json(&out.return_value, max_output_bytes) {
                    Ok(json) => (Some(json), RunStatus::Ok, out.gas_used, out.memory_used),
                    Err(e) => (
                        None,
                        error_status(&e, prelude_lines),
                        out.gas_used,
                        out.memory_used,
                    ),
                },
                Err(e) => (
                    None,
                    error_status(&e, prelude_lines),
                    vm.gas_used(),
                    vm.memory_used(),
                ),
            };
            let transcript = vm.transcript();
            let log = vm.host().log();
            assert_eq!(
                transcript.len(),
                log.len(),
                "bug: core recorded {} tool calls but the host saw {}",
                transcript.len(),
                log.len()
            );
            let entries = transcript
                .iter()
                .zip(log)
                .map(|(record, (decision, provenance))| TraceEntry {
                    record: record.clone(),
                    decision: decision.clone(),
                    provenance: provenance.clone(),
                })
                .collect::<Vec<_>>();
            (entries, output, status, gas_used, memory_used)
        })
        .await;
        let (entries, output, status, gas_used, memory_used) = match run {
            Ok(run) => run,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        };

        let mut trace = Trace {
            header: TraceHeader {
                trace_id: trace_id.clone(),
                session: req.session,
                principal: principal.to_string(),
                program_hash,
                policy_hash: hex::encode(self.policy.policy_hash()),
                description_hash,
                vm_version: crate::VM_VERSION.to_string(),
                vm_config: self.vm.clone(),
                request: req.request,
            },
            entries,
            footer: TraceFooter {
                output,
                status: status.clone(),
                gas_used,
                memory_used,
                signature: String::new(),
            },
        };
        trace.sign(&self.signing_key);
        self.store
            .put_trace(&trace)
            .expect("trace store: write trace");

        Ok(ExecuteResponse {
            result: trace
                .footer
                .output
                .as_deref()
                .map(|json| serde_json::from_str(json).expect("canonical output is valid JSON")),
            trace_id,
            status,
        })
    }

    fn allowed_schemas(&self, principal: &str) -> Vec<ToolSchema> {
        let allowed = self.policy.allowed_tools(principal);
        self.downstreams
            .tools()
            .iter()
            .filter(|t| allowed.contains(&t.qualified_name()))
            .cloned()
            .collect()
    }
}

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Names the dialect already gives a meaning to: core's globals and call-position
/// builtins, and the identifiers its parser rejects. A server named after one
/// would fail to compile or shadow it for the agent's program.
const DIALECT_NAMES: &[&str] = &[
    "tool",
    "string",
    "math",
    "table",
    "json",
    "tostring",
    "tonumber",
    "type",
    "select",
    "unpack",
    "pairs_sorted",
    "pairs",
    "ipairs",
    "pcall",
    "error",
    "log",
    "print",
    "debug",
    "io",
    "os",
    "package",
    "require",
    "load",
    "dofile",
    "loadfile",
    "loadstring",
    "collectgarbage",
    "setmetatable",
    "getmetatable",
    "rawget",
    "rawset",
    "setfenv",
    "getfenv",
    "coroutine",
];

/// The prelude declares `local <server> = {}` and `<server>.<tool> = ...`, so
/// the server must be a free Lua name and the tool a valid field name.
fn check_lua_names(tool: &ToolSchema) -> Result<(), String> {
    let is_name = |s: &str| {
        let mut chars = s.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !LUA_KEYWORDS.contains(&s)
    };
    let qualified = tool.qualified_name();
    if !is_name(&tool.server) || DIALECT_NAMES.contains(&tool.server.as_str()) {
        return Err(format!(
            "tool {qualified}: downstream name `{}` is not usable as a Lua name",
            tool.server
        ));
    }
    if !is_name(&tool.name) {
        return Err(format!(
            "tool {qualified}: tool name `{}` is not usable as a Lua field name",
            tool.name
        ));
    }
    Ok(())
}

/// Canonical JSON of the return value, which is what the trace commits to.
/// Core does not enforce `max_output_bytes`, so the gateway does, here.
fn output_json(value: &LuaValue, max_output_bytes: usize) -> Result<String, VmError> {
    let bytes = canonical_serialize(value).map_err(VmError::from)?;
    if bytes.len() > max_output_bytes {
        return Err(VmError::OutputExceeded);
    }
    Ok(String::from_utf8(bytes).expect("canonical JSON is ASCII"))
}

/// `kind` is the `VmError` variant. A line inside the program is reported
/// against the agent's own numbering; a line inside the generated prelude is
/// dropped, since the agent never wrote it.
fn error_status(e: &VmError, prelude_lines: u32) -> RunStatus {
    let (kind, message) = match e {
        VmError::WithLine(line, inner) => {
            let RunStatus::Error { kind, message } = error_status(inner, prelude_lines) else {
                unreachable!("error_status always returns an error")
            };
            let message = if *line > prelude_lines {
                format!("line {}: {message}", line - prelude_lines)
            } else {
                message
            };
            return RunStatus::Error { kind, message };
        }
        VmError::GasExhausted => ("GasExhausted", "gas exhausted".to_string()),
        VmError::MemoryExhausted => ("MemoryExhausted", "memory exhausted".to_string()),
        VmError::CallDepthExceeded => ("CallDepthExceeded", "call depth exceeded".to_string()),
        VmError::TypeError(m) => ("TypeError", m.clone()),
        VmError::RuntimeError(value) => ("RuntimeError", lua_message(value)),
        VmError::ToolError(m) => ("ToolError", m.clone()),
        VmError::OutputExceeded => ("OutputExceeded", "output exceeded".to_string()),
    };
    RunStatus::Error {
        kind: kind.to_string(),
        message,
    }
}

fn lua_message(value: &LuaValue) -> String {
    match value {
        LuaValue::String(s) => String::from_utf8_lossy(s.as_bytes()).into_owned(),
        other => match canonical_serialize(other) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => format!("{other:?}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proveno::types::value::LuaString;

    fn error(e: VmError) -> (String, String) {
        match error_status(&e, 3) {
            RunStatus::Error { kind, message } => (kind, message),
            RunStatus::Ok => panic!("expected an error"),
        }
    }

    #[test]
    fn line_in_program_is_reported_against_the_agent() {
        let e = VmError::WithLine(5, Box::new(VmError::ToolError("policy: no".into())));
        assert_eq!(error(e), ("ToolError".into(), "line 2: policy: no".into()));
    }

    #[test]
    fn line_in_prelude_is_dropped() {
        let e = VmError::WithLine(2, Box::new(VmError::GasExhausted));
        assert_eq!(error(e), ("GasExhausted".into(), "gas exhausted".into()));
    }

    #[test]
    fn runtime_error_value_becomes_the_message() {
        let e = VmError::RuntimeError(LuaValue::String(LuaString::from_str("boom")));
        assert_eq!(error(e), ("RuntimeError".into(), "boom".into()));
        let e = VmError::RuntimeError(LuaValue::Integer(7));
        assert_eq!(error(e), ("RuntimeError".into(), "7".into()));
    }

    fn tool(server: &str, name: &str) -> ToolSchema {
        ToolSchema {
            server: server.into(),
            name: name.into(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object" }),
            output_schema: None,
        }
    }

    #[test]
    fn lua_names_are_accepted() {
        for (server, name) in [
            ("wallet", "get_balance"),
            ("_x2", "type"),
            ("market", "Get2"),
        ] {
            assert_eq!(
                check_lua_names(&tool(server, name)),
                Ok(()),
                "{server}.{name}"
            );
        }
    }

    #[test]
    fn names_lua_cannot_declare_are_rejected() {
        for (server, name) in [
            ("wallet", "get-price"),
            ("wallet", "end"),
            ("wallet", "2fa"),
            ("my-wallet", "get"),
            ("local", "get"),
            ("tool", "get"),
            ("string", "get"),
            ("os", "get"),
        ] {
            assert!(
                check_lua_names(&tool(server, name)).is_err(),
                "{server}.{name}"
            );
        }
    }

    /// Accepted names, including a tool named after a builtin, compile as a
    /// prelude alongside a program that uses that builtin.
    #[test]
    fn accepted_names_compile_in_a_prelude() {
        let allowed = [tool("_x2", "type"), tool("wallet", "get_balance")];
        let prelude = description::build(&allowed).prelude;
        compile_program(&prelude, "return type(1)").unwrap();
    }

    #[test]
    fn output_over_the_limit_is_output_exceeded() {
        let value = LuaValue::String(LuaString::from_str("0123456789"));
        assert_eq!(output_json(&value, 64).unwrap(), "\"0123456789\"");
        assert!(matches!(
            output_json(&value, 11),
            Err(VmError::OutputExceeded)
        ));
    }
}
