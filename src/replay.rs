//! Offline replay of a stored trace (spec section 3.5).
//!
//! Replay reads only the store and the signing key. It re-runs the stored
//! program over core's strict `TapeHost`, built from the trace's records, with
//! the VM limits recorded in the header, and compares the result to the footer.

use anyhow::Context;
use proveno::bytecode::verify;
use proveno::compiler::compile;
use proveno::compiler::program_hash::compute_program_hash_sha256;
use proveno::parser::parse;
use proveno::types::value::LuaValue;
use proveno::{OracleTape, TapeCall, TapeHost, ToolCallRecord, Vm, VmConfig};
use serde::{Deserialize, Serialize};

use crate::config::GatewayConfig;
use crate::engine::{error_status, output_json};
use crate::store::TraceStore;
use crate::trace::{RunStatus, signing_key_from_hex};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub trace_id: String,
    /// True only when `mismatches` is empty.
    pub matched: bool,
    pub mismatches: Vec<String>,
    /// What the replay produced. A line number in an error message is counted
    /// against the stored source, prelude included.
    pub output: Option<String>,
    pub status: RunStatus,
    pub gas_used: u64,
    pub memory_used: u64,
}

/// Replays `trace_id` with no downstream connection. An unreadable or
/// unverifiable trace, or a stored program that no longer compiles, is an
/// error; every way the replay departs from the recording is a mismatch.
pub fn replay(config: &GatewayConfig, trace_id: &str) -> anyhow::Result<ReplayReport> {
    let key = signing_key_from_hex(
        &config
            .server
            .signing_key
            .resolve()
            .context("server.signing_key")?,
    )
    .context("server.signing_key")?;
    let store = TraceStore::open(&config.store.dir)
        .with_context(|| format!("store {}", config.store.dir.display()))?;
    let trace = store
        .get_trace(trace_id)
        .with_context(|| format!("trace {trace_id}"))?;
    trace
        .verify(&key.verifying_key())
        .with_context(|| format!("trace {trace_id}: signature verification failed"))?;
    let header = &trace.header;
    let footer = &trace.footer;
    let mut mismatches = Vec::new();

    // The stored source already includes the prelude, so it compiles as is.
    let source = store
        .get_program(&header.program_hash)
        .with_context(|| format!("program {}", header.program_hash))?;
    let block = parse(&source).map_err(|e| {
        anyhow::anyhow!(
            "program {}: line {}: {}",
            header.program_hash,
            e.span().line,
            e.message()
        )
    })?;
    let program = compile(&block)
        .map_err(|e| anyhow::anyhow!("program {}: {}", header.program_hash, e.message()))?;
    verify(&program).map_err(|e| {
        anyhow::anyhow!(
            "program {}: bytecode verification: {e:?}",
            header.program_hash
        )
    })?;
    let program_hash = hex::encode(compute_program_hash_sha256(&program.prototypes));
    if program_hash != header.program_hash {
        mismatches.push(format!(
            "program_hash: header has {}, stored source compiles to {program_hash}",
            header.program_hash
        ));
    }

    let records: Vec<ToolCallRecord> = trace.entries.iter().map(|e| e.record.clone()).collect();
    let tape = OracleTape::from_records(&records);
    let max_output_bytes = header.vm_config.max_output_bytes;
    let mut vm = Vm::new(VmConfig::from(&header.vm_config), TapeHost::strict(tape));
    // Same mapping as the engine. Only the status kind is compared, so the
    // prelude line offset does not matter here.
    let (output, status, gas_used, memory_used) = match vm.execute(&program, LuaValue::Nil) {
        Ok(out) => match output_json(&out.return_value, max_output_bytes) {
            Ok(json) => (Some(json), RunStatus::Ok, out.gas_used, out.memory_used),
            Err(e) => (None, error_status(&e, 0), out.gas_used, out.memory_used),
        },
        Err(e) => (None, error_status(&e, 0), vm.gas_used(), vm.memory_used()),
    };

    let host = vm.host();
    if let Some(d) = host.divergence() {
        mismatches.push(format!(
            "divergence at seq {}: expected {}, actual {}",
            d.seq,
            d.expected
                .as_ref()
                .map_or_else(|| "no recorded call".to_string(), describe_call),
            describe_call(&d.actual)
        ));
    }
    if host.remaining() > 0 {
        mismatches.push(format!(
            "tape: {} of {} recorded calls left unconsumed",
            host.remaining(),
            records.len()
        ));
    }
    if output != footer.output {
        mismatches.push(format!(
            "output: recorded {}, replayed {}",
            footer.output.as_deref().unwrap_or("none"),
            output.as_deref().unwrap_or("none")
        ));
    }
    if status_kind(&status) != status_kind(&footer.status) {
        mismatches.push(format!(
            "status: recorded {}, replayed {}",
            status_kind(&footer.status),
            status_kind(&status)
        ));
    }
    if gas_used != footer.gas_used {
        mismatches.push(format!(
            "gas_used: recorded {}, replayed {gas_used}",
            footer.gas_used
        ));
    }
    if memory_used != footer.memory_used {
        mismatches.push(format!(
            "memory_used: recorded {}, replayed {memory_used}",
            footer.memory_used
        ));
    }

    Ok(ReplayReport {
        trace_id: trace_id.to_string(),
        matched: mismatches.is_empty(),
        mismatches,
        output,
        status,
        gas_used,
        memory_used,
    })
}

fn describe_call(call: &TapeCall) -> String {
    format!(
        "{} {}",
        call.tool_name,
        String::from_utf8_lossy(&call.args_canonical)
    )
}

/// The status compared by replay. The message is not: a source that differs
/// from the stored one only in comments or line numbers shares its
/// `program_hash`, and its error message quotes its own line numbers.
pub fn status_kind(status: &RunStatus) -> &str {
    match status {
        RunStatus::Ok => "ok",
        RunStatus::Error { kind, .. } => kind,
    }
}
