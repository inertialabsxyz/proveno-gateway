//! proveno-gateway: an MCP gateway that runs agent-written Lua programs on the
//! proveno runtime, policy-checks and records every tool call, and replays any
//! run bit-for-bit.
//!
//! The spec is `planning/proveno-gateway-spec.md` in the proveno umbrella.

pub mod config;
pub mod description;
pub mod dialect;
pub mod downstream;
pub mod engine;
pub mod host;
pub mod policy;
pub mod replay;
pub mod schema;
pub mod server;
pub mod store;
pub mod trace;
pub mod values;

/// The proveno-core release the VM comes from, recorded in every trace header.
pub const VM_VERSION: &str = "proveno-core v0.3.0";
