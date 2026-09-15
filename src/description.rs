//! The generated tool description and the Lua prelude that goes with it.

// Phase 2c stub: build

use serde::{Deserialize, Serialize};

use crate::downstream::ToolSchema;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescription {
    pub text: String,
    pub hash: [u8; 32],
    pub prelude: String,
}

/// The placeholder tool the examples are written against, as `(server, tool)`.
pub const EXAMPLE_TOOL: (&str, &str) = ("example", "lookup");

/// Fixed example programs. They use a placeholder tool, not the real allow-list,
/// so the text stays stable.
pub const EXAMPLES: [&str; 3] = [
    // Calling a tool with table-call sugar and returning a field.
    "\
local item = example.lookup{ id = \"a1\" }
return item.status",
    // Catching a denied or failed call and reporting it.
    "\
local ok, res = pcall(function()
  return example.lookup{ id = \"a1\" }
end)
if not ok then
  return { ok = false, error = res }
end
return { ok = true, status = res.status }",
    // Several calls, returning a table as the result.
    "\
local ids = { \"a1\", \"b2\" }
local statuses = {}
for _, id in ipairs(ids) do
  local item = example.lookup{ id = id }
  statuses[id] = item.status
end
return { count = #ids, statuses = statuses }",
];

/// The examples section of the description and of the lua-guide.
pub fn examples_section() -> String {
    let (server, tool) = EXAMPLE_TOOL;
    let mut out = format!(
        "# Examples\n\n`{server}.{tool}{{ id: string }} -> table` is a placeholder; \
         use the tools listed in the tool API.\n"
    );
    for example in EXAMPLES {
        out.push_str("\n```lua\n");
        out.push_str(example);
        out.push_str("\n```\n");
    }
    out
}

pub fn build(_allowed: &[ToolSchema]) -> ToolDescription {
    todo!("Phase 2c")
}
