//! The Lua dialect rules, the lua-guide, and compiling a program with its
//! prelude.

use proveno::bytecode::verify;
use proveno::compiler::{CompileError, compile, proto::CompiledProgram};
use proveno::parser::{lexer::ParseError, parse};
use serde::{Deserialize, Serialize};

pub const DIALECT_VERSION: &str = "2";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct LintError {
    pub line: u32,
    pub message: String,
}

/// The dialect rules, as proveno-core v0.3.0 enforces them. Changing this text
/// changes every description hash, so bump `DIALECT_VERSION` with it.
///
/// Every claim here is read off the pinned core, not off standard Lua: the
/// library list is `build_string_module`, `build_math_module`,
/// `build_table_module` and `build_json_module` in `src/vm/builtins.rs`, and
/// the globals are the names `compile_name` resolves in `src/compiler/
/// codegen.rs`. `tests/dialect.rs` runs every name listed below.
///
/// Two sentences are dated on purpose:
///
/// - The fourth `plain` argument to `string.find` is ignored by the pinned
///   core, which then refuses the pattern metacharacter anyway. Step 12 of the
///   agent prompts makes the flag do a literal search; when that core is
///   pinned here, say so instead of sending the model to `find_literal`.
/// - Decimal strings are split by hand. Step 7 adds `decimal.parse`; when it
///   lands, that sentence becomes one call.
const DIALECT_RULES: &str = "\
# Lua dialect (version 2)

Programs are a restricted, deterministic Lua. The rules below are enforced
before the program runs; a violation is returned as a line-numbered error.

- Not available (rejected at parse time): debug, io, os, package, require,
  load, dofile, loadfile, loadstring, collectgarbage, setmetatable,
  getmetatable, rawget, rawset, setfenv, getfenv, coroutine.
- Integers only. There are no floats and no float literals. Non-integer numbers
  returned by tools arrive as decimal strings, such as \"2500.75\". Use `//` for
  division; `/` is not supported.
- No user-defined globals. Declare every variable and function `local`; the
  only globals are the library names listed below.
- Iterate tables with `pairs_sorted(t)`; `pairs` is also sorted, and `ipairs`
  walks arrays.
- Every call returns exactly one value, and a function returns one value:
  `return a, b` does not compile. The one two-value form in the language is
  `local ok, err = pcall(function() ... end)`.
- Functions take a fixed parameter list; `...` is rejected.
- Call library functions by name: `string.sub(s, 1, 4)`. The colon form
  `s:sub(1, 4)` is a type error, because strings have no methods.
- The standard library is this list and nothing else. Any other name under
  `string`, `math`, `table` or `json` is nil, and calling it fails.
  - string: `len`, `sub`, `find`, `find_literal`, `upper`, `lower`, `rep`,
    `byte`, `char`, `format`.
  - math: `abs`, `min`, `max`, `scale_div(a, b, scale)` (`a * scale / b`,
    truncated towards zero), `maxinteger`, `mininteger`.
  - table: `insert`, `remove`, `concat`, `sort`, `move`.
  - json: `encode`, `decode`, `decode_strings`. `decode` rejects a number with
    a fractional part or an exponent; `decode_strings` returns every number as
    its source text.
  - Globals: `pcall`, `error`, `type`, `tostring`, `tonumber`, `select`,
    `unpack`, `pairs_sorted`, `pairs`, `ipairs`, `log`, `print`. `type`
    answers \"integer\" for a number, never \"number\".
- There are no string patterns. `string.match`, `string.gmatch` and
  `string.gsub` exist but every call fails at run time.
  `string.find(s, needle [, init])` searches for a literal and fails if
  `needle` holds any of `^ $ ( ) % . [ ] * + - ?`; its fourth argument is
  ignored, so search for those with `string.find_literal(s, needle [, init])`.
  Both return the 1-based start index only, or nil.
- `string.format` takes `%d`, `%s`, `%x` and `%%`, with no width or precision.
- `tonumber` parses whole numbers only: it returns nil for \"2500.75\". To use a
  decimal string, find the dot with `string.find_literal(s, \".\")` and cut it
  with `string.sub`, then work in scaled integers.
- Time and randomness exist only as tool calls, so they are recorded.
- A tool is called by the exact name given in the Tool API below, as
  `<downstream>.<tool>{ arg = value }`. Those two names are placeholders for
  the ones listed there: if the API lists `market.get_price`, write
  `market.get_price{ pair = \"ETH/USD\" }`, not `server.market.get_price{...}`
  and not `tool.call(...)`. A failed or denied call raises an error; catch it
  with `local ok, res = pcall(function() return market.get_price{ ... } end)`.
  `pcall(tool.call, ...)` is not supported.
- Join strings with `..`, as in `\"tx \" .. hash`. `string.format` is available
  for widths and padding.
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
    fn dialect_rules_name_find_literal_and_do_not_promise_a_whole_module() {
        let rules = dialect_rules();
        assert!(rules.contains("find_literal"));
        for wildcard in ["`string.*`", "`math.*`", "`table.*`", "`json.*`"] {
            assert!(!rules.contains(wildcard), "still promises {wildcard}");
        }
        for unsupported in ["`string.match`", "`string.gmatch`", "`string.gsub`"] {
            assert!(rules.contains(unsupported), "missing {unsupported}");
        }
        assert!(rules.contains("There are no string patterns."));
        assert!(rules.contains("`return a, b` does not compile"));
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
