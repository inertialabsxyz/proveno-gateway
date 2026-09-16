//! The gateway's `HostInterface`: schema, policy, credential, dispatch, record.
//!
//! Credentials are attached by `Downstreams` at connect time, so the host never
//! handles them. What it records per call is the gateway's decision and the
//! response's provenance tag; core records the call itself, with the attestation
//! blob the host hands it through `take_attestation`.
//!
//! Provenance is bind-only. The host passes on what the downstream reported and
//! checks nothing about whether it is true.

use std::sync::Arc;

use proveno::HostInterface;
use proveno::host::canonicalize::canonical_serialize_table;
use proveno::types::table::LuaTable;
use tokio::runtime::Handle;

use crate::downstream::{Downstreams, ToolResponse, ToolSchema};
use crate::policy::{Decision, Policy, SessionState};
use crate::schema::check_args;
use crate::trace::{CallDecision, Provenance};
use crate::values::{json_to_table, table_to_json};

pub struct GatewayHost {
    principal: String,
    downstreams: Arc<Downstreams>,
    allowed: Arc<Vec<ToolSchema>>,
    policy: Arc<Policy>,
    handle: Handle,
    log: Vec<(CallDecision, Provenance)>,
    /// The attestation for the call that just succeeded, until core takes it.
    attestation: Option<Vec<u8>>,
}

impl GatewayHost {
    /// `handle` must belong to a runtime that is not driven from the calling
    /// thread: `call_tool` blocks on it, so run the VM in `spawn_blocking`.
    pub fn new(
        principal: String,
        downstreams: Arc<Downstreams>,
        allowed: Arc<Vec<ToolSchema>>,
        policy: Arc<Policy>,
        handle: Handle,
    ) -> Self {
        GatewayHost {
            principal,
            downstreams,
            allowed,
            policy,
            handle,
            log: Vec::new(),
            attestation: None,
        }
    }

    /// One `(decision, provenance)` per call that reached the host, in call
    /// order. Core records exactly the same calls, so this zips with the
    /// transcript. A call that failed, at any stage, is `Unsigned`: no
    /// response was accepted, so there is nothing to attest.
    pub fn log(&self) -> &[(CallDecision, Provenance)] {
        &self.log
    }

    fn refuse(&mut self, decision: CallDecision, message: String) -> Result<LuaTable, String> {
        self.log.push((decision, Provenance::Unsigned));
        Err(message)
    }
}

impl HostInterface for GatewayHost {
    fn call_tool(&mut self, name: &str, args: &LuaTable) -> Result<LuaTable, String> {
        self.attestation = None;
        let args = match table_to_json(args) {
            Ok(args) => args,
            Err(e) => {
                let reason = format!("{name}: {e}");
                return self.refuse(
                    CallDecision::RejectedBySchema {
                        reason: reason.clone(),
                    },
                    format!("schema: {reason}"),
                );
            }
        };

        let Some(tool) = self.allowed.iter().find(|t| t.qualified_name() == name) else {
            let reason = format!("tool {name} is not allowed for {}", self.principal);
            return self.refuse(
                CallDecision::DeniedByPolicy {
                    reason: reason.clone(),
                },
                format!("policy: {reason}"),
            );
        };

        if let Err(reason) = check_args(tool, &args) {
            return self.refuse(
                CallDecision::RejectedBySchema {
                    reason: reason.clone(),
                },
                format!("schema: {reason}"),
            );
        }

        if let Decision::Deny { reason } =
            self.policy
                .check(&self.principal, name, &args, &SessionState {})
        {
            return self.refuse(
                CallDecision::DeniedByPolicy {
                    reason: reason.clone(),
                },
                format!("policy: {reason}"),
            );
        }

        let response = self.handle.block_on(self.downstreams.call(name, args));
        match accept(name, response) {
            Ok((table, provenance, attestation)) => {
                self.log.push((CallDecision::Allowed, provenance));
                self.attestation = Some(attestation).filter(|a| !a.is_empty());
                Ok(table)
            }
            Err(e) => {
                self.log.push((CallDecision::Allowed, Provenance::Unsigned));
                Err(e)
            }
        }
    }

    /// Core asks for this once, straight after a call that returned `Ok`, so a
    /// blob is handed over at most once and only for the call it belongs to.
    fn take_attestation(&mut self) -> Option<Vec<u8>> {
        self.attestation.take()
    }
}

/// Turns a dispatched call into the table the program sees, plus the
/// provenance tag and attestation blob that only the trace sees.
fn accept(
    name: &str,
    response: Result<ToolResponse, String>,
) -> Result<(LuaTable, Provenance, Vec<u8>), String> {
    let response = response.map_err(|e| format!("downstream: {e}"))?;
    let table = json_to_table(&response.value).map_err(|e| format!("downstream: {name}: {e}"))?;

    // Core records a response only once it has canonicalised it, and a
    // response it cannot canonicalise (too deep, too long) would leave the
    // call out of the transcript and the run unreplayable. Refusing it here
    // turns it into a recorded tool error instead.
    canonical_serialize_table(&table)
        .map_err(|e| format!("downstream: {name}: response cannot be recorded: {e:?}"))?;
    Ok((table, response.provenance, response.attestation))
}
