//! The Lua dialect rules, the lua-guide, and compiling a program with its
//! prelude.

use proveno::bytecode::verify;
use proveno::compiler::{CompileError, compile, proto::CompiledProgram};
use proveno::parser::{lexer::ParseError, parse};
use serde::{Deserialize, Serialize};

pub const DIALECT_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct LintError {
    pub line: u32,
    pub message: String,
}

/// The dialect rules, as proveno-core v0.2.0 enforces them. Changing this text
/// changes every description hash, so bump `DIALECT_VERSION` with it.
const DIALECT_RULES: &str = "\
# Lua dialect (version 1)

Programs are a restricted, deterministic Lua. The rules below are enforced
before the program runs; a violation is returned as a line-numbered error.

- Not available (rejected at parse time): debug, io, os, package, require,
  load, dofile, loadfile, loadstring, collectgarbage, setmetatable,
  getmetatable, rawget, rawset, setfenv, getfenv, coroutine.
- Integers only. There are no floats and no float literals. Non-integer numbers
  returned by tools arrive as decimal strings, such as \"2500.75\". Use `//` for
  division; `/` is not supported.
- No globals. Declare every variable and function `local`.
- Iterate tables with `pairs_sorted(t)`; `pairs` is also sorted, and `ipairs`
  walks arrays.
- Standard library: `string.*`, `math.*`, `table.*`, `json.*`, `pcall`,
  `error`, `type`, `pairs_sorted`, `pairs`, `ipairs`, `log`, `print`.
- Time and randomness exist only as tool calls, so they are recorded.
- Tools are called as `server.tool{ arg = value }`. A failed or denied tool call
  raises an error; catch it with
  `local ok, res = pcall(function() return server.tool{ ... } end)`.
  `pcall(tool.call, ...)` is not supported.
- The value of the final `return` is the result.
";

pub fn dialect_rules() -> &'static str {
    DIALECT_RULES
}

/// The dialect rules followed by the fixed example programs. Independent of any
/// downstream's tools.
pub fn lua_guide() -> String {
    format!(
        "{}\n{}",
        dialect_rules(),
        crate::description::examples_section()
    )
}

/// Parse, compile and verify `prelude` followed by `program`. Errors are
/// reported against the program's own line numbers.
pub fn compile_program(prelude: &str, program: &str) -> Result<CompiledProgram, LintError> {
    let mut source = String::with_capacity(prelude.len() + program.len() + 1);
    source.push_str(prelude);
    if !source.is_empty() && !source.ends_with('\n') {
        source.push('\n');
    }
    let offset = source.matches('\n').count() as u32;
    source.push_str(program);

    let located = |line: u32, message: String| {
        if line <= offset {
            panic!(
                "generated prelude failed to compile at line {line}: {message}\n\
                 --- prelude ---\n{prelude}"
            );
        }
        LintError {
            line: line - offset,
            message,
        }
    };

    let block = parse(&source).map_err(|e| located(e.span().line, parse_message(&e)))?;
    let compiled = compile(&block).map_err(|e| {
        let line = e.line();
        let message = compile_message(&e, program_line(program, line.saturating_sub(offset)));
        located(line, message)
    })?;
    // The verifier reports no source line. A failure here is a core compiler
    // bug rather than a mistake the model can fix, so report it at line 0.
    verify(&compiled).map_err(|e| LintError {
        line: 0,
        message: format!("bytecode verification failed: {e:?}"),
    })?;
    Ok(compiled)
}

fn parse_message(e: &ParseError) -> String {
    match e {
        ParseError::DisallowedIdent { name, .. }
            if matches!(name.as_str(), "os" | "io" | "require" | "load") =>
        {
            format!("`{name}` is not available; time and randomness are tool calls")
        }
        ParseError::FloatLiteral { .. } => {
            "floats are not supported; use integers or decimal strings".into()
        }
        other => other.message(),
    }
}

fn compile_message(e: &CompileError, source_line: &str) -> String {
    match e {
        CompileError::UnknownGlobal { name, .. } => {
            format!("`{name}` is not defined; declare it `local`, or check the tool API")
        }
        CompileError::ToolAsValue { .. } | CompileError::IndirectToolCall { .. }
            if is_pcall_tool_call(source_line) =>
        {
            "`pcall(tool.call, ...)` is not supported; use \
             `pcall(function() return tool.call(name, args) end)`"
                .into()
        }
        other => {
            // Core prefixes its compile messages with "line N: "; the line is
            // reported separately, and against the program, so drop it.
            let message = other.message();
            match message.split_once(": ") {
                Some((prefix, rest)) if prefix.starts_with("line ") => rest.to_string(),
                _ => message,
            }
        }
    }
}

fn program_line(program: &str, line: u32) -> &str {
    (line as usize)
        .checked_sub(1)
        .and_then(|i| program.lines().nth(i))
        .unwrap_or("")
}

/// Whether a source line contains `pcall(tool.call` with any spacing.
fn is_pcall_tool_call(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains("pcall(tool.call,") || compact.contains("pcall(tool.call)")
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

    #[test]
    fn dialect_rules_name_the_version_and_every_rejected_identifier() {
        let rules = dialect_rules();
        assert!(rules.contains(&format!("version {DIALECT_VERSION}")));
        for name in [
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
        ] {
            assert!(rules.contains(name), "missing {name}");
        }
    }

    #[test]
    fn errors_are_reported_against_program_lines() {
        let prelude = "local a = {}\nlocal b = {}\n";
        let err = compile_program(prelude, "local x = 1\nreturn y").unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn prelude_without_trailing_newline_is_separated() {
        let err = compile_program("local a = {}", "return y").unwrap_err();
        assert_eq!(err.line, 1);
    }

    #[test]
    fn other_compile_messages_lose_the_core_line_prefix() {
        let err = compile_program("", "\nbreak").unwrap_err();
        assert_eq!(err.line, 2);
        assert_eq!(err.message, "`break` outside loop");
    }

    #[test]
    #[should_panic(expected = "generated prelude failed to compile")]
    fn broken_prelude_panics() {
        let _ = compile_program("local a = undefined_thing\n", "return 1");
    }

    #[test]
    fn lua_guide_starts_with_the_rules_and_includes_examples() {
        let guide = lua_guide();
        assert!(guide.starts_with(dialect_rules()));
        assert!(guide.contains("# Examples"));
    }
}
