//! Mapping between JSON values and the VM's integer-only value model.

// Phase 2b stub

use proveno::types::table::LuaTable;

pub fn table_to_json(_t: &LuaTable) -> Result<serde_json::Value, String> {
    todo!("Phase 2b")
}

pub fn json_to_table(_v: &serde_json::Value) -> Result<LuaTable, String> {
    todo!("Phase 2b")
}
