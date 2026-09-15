//! The Lua dialect rules, the lua-guide, and compiling a program with its
//! prelude.

// Phase 2c stub

use proveno::compiler::proto::CompiledProgram;
use serde::{Deserialize, Serialize};

pub const DIALECT_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct LintError {
    pub line: u32,
    pub message: String,
}

pub fn dialect_rules() -> &'static str {
    todo!("Phase 2c")
}

pub fn lua_guide() -> String {
    todo!("Phase 2c")
}

pub fn compile_program(_prelude: &str, _program: &str) -> Result<CompiledProgram, LintError> {
    todo!("Phase 2c")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_error_displays_with_line_number() {
        let e = LintError {
            line: 4,
            message: "os is not available; call clock.now{}".into(),
        };
        assert_eq!(
            e.to_string(),
            "line 4: os is not available; call clock.now{}"
        );
    }
}
