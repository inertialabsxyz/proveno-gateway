//! The gateway's `HostInterface`: schema, policy, credential, dispatch, record.
//!
//! Credentials are attached by `Downstreams` at connect time, so the host never
//! handles them. What it records per call is the gateway's decision and the
//! response's provenance; core records the call itself.

use std::sync::Arc;

use proveno::HostInterface;
use proveno::host::canonicalize::canonical_serialize_table;
use proveno::types::table::LuaTable;
use tokio::runtime::Handle;

use crate::downstream::{Downstreams, ToolSchema};
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
        }
    }

    /// One `(decision, provenance)` per call that reached the host, in call
    /// order. Core records exactly the same calls, so this zips with the
    /// transcript.
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
        self.log.push((CallDecision::Allowed, Provenance::Unsigned));
        let table = json_to_table(&response.map_err(|e| format!("downstream: {e}"))?)
            .map_err(|e| format!("downstream: {name}: {e}"))?;

        // Core records a response only once it has canonicalised it, and a
        // response it cannot canonicalise (too deep, too long) would leave the
        // call out of the transcript and the run unreplayable. Refusing it here
        // turns it into a recorded tool error instead.
        canonical_serialize_table(&table)
            .map_err(|e| format!("downstream: {name}: response cannot be recorded: {e:?}"))?;
        Ok(table)
    }
}
