//! The dialect rules against the pinned proveno-core: every library name the
//! rules list is registered, every name they call out as missing or broken
//! really is, and the idioms they recommend run.
//!
//! These tests are the reason the text can be trusted. A core bump that adds or
//! drops a builtin fails here, not in front of a model.

use proveno::{
    HostInterface, Vm, VmConfig,
    types::{
        table::LuaTable,
        value::{LuaString, LuaValue},
    },
};
use proveno_gateway::dialect::{LintError, compile_program, dialect_rules};

/// No tools: these programs only exercise the language and its library.
struct NoTools;

impl HostInterface for NoTools {
    fn call_tool(&mut self, name: &str, _args: &LuaTable) -> Result<LuaTable, String> {
        panic!("unexpected tool call: {name}")
    }
}

fn run(program: &str) -> LuaValue {
    let compiled = compile_program("", program)
        .unwrap_or_else(|e| panic!("failed to compile: {e}\n{program}"));
    Vm::new(VmConfig::default(), NoTools)
        .execute(&compiled, LuaValue::Nil)
        .unwrap_or_else(|e| panic!("failed at run time: {e:?}\n{program}"))
        .return_value
}

fn lint(program: &str) -> LintError {
    compile_program("", program).expect_err("expected a lint error")
}

fn string(s: &str) -> LuaValue {
    LuaValue::String(LuaString::from_str(s))
}

/// `(module, name, type)` for every library member the rules list.
const LIBRARY: [(&str, &str, &str); 27] = [
    ("string", "len", "function"),
    ("string", "sub", "function"),
    ("string", "find", "function"),
    ("string", "find_literal", "function"),
    ("string", "upper", "function"),
    ("string", "lower", "function"),
    ("string", "rep", "function"),
    ("string", "byte", "function"),
    ("string", "char", "function"),
    ("string", "format", "function"),
    ("math", "abs", "function"),
    ("math", "min", "function"),
    ("math", "max", "function"),
    ("math", "scale_div", "function"),
    ("math", "maxinteger", "integer"),
    ("math", "mininteger", "integer"),
    ("table", "insert", "function"),
    ("table", "remove", "function"),
    ("table", "concat", "function"),
    ("table", "sort", "function"),
    ("table", "move", "function"),
    ("json", "encode", "function"),
    ("json", "decode", "function"),
    ("json", "decode_strings", "function"),
    ("decimal", "parse", "function"),
    ("decimal", "format", "function"),
    ("decimal", "rescale", "function"),
];

#[test]
fn every_library_name_in_the_rules_is_registered_by_core() {
    for (module, name, kind) in LIBRARY {
        assert_eq!(
            run(&format!("return type({module}.{name})")),
            string(kind),
            "{module}.{name} is listed in the rules but core does not register it"
        );
        // Listed as `name`, or as `name(args)` where the signature matters.
        let rules = dialect_rules();
        assert!(
            rules.contains(&format!("`{name}`")) || rules.contains(&format!("`{name}(")),
            "{module}.{name} is registered but the rules do not name it"
        );
    }
}

#[test]
fn a_name_the_rules_leave_out_is_nil() {
    // Standard Lua has the first four, and `decimal.round` is a plausible
    // guess. Core has none of them, which is why the rules list members rather
    // than `string.*`.
    for absent in [
        "math.floor",
        "math.fmod",
        "string.gfind",
        "table.unpack",
        "decimal.round",
    ] {
        assert_eq!(run(&format!("return type({absent})")), string("nil"));
    }
}

#[test]
fn match_gmatch_and_gsub_exist_but_fail_at_run_time() {
    for call in [
        "string.match(\"ab\", \"a\")",
        "string.gmatch(\"ab\", \"a\")",
        "string.gsub(\"ab\", \"a\", \"b\")",
    ] {
        assert_eq!(
            run(&format!("return type({})", call_target(call))),
            string("function")
        );
        let program = format!(
            "local ok, err = pcall(function() return {call} end)\nreturn {{ ok = ok, err = err }}"
        );
        let out = run(&program);
        assert_eq!(
            field(&out, "ok"),
            LuaValue::Boolean(false),
            "{call} succeeded"
        );
        assert_eq!(
            field(&out, "err"),
            string("string.match/gmatch/gsub not supported")
        );
    }
}

/// `string.match("ab", "a")` -> `string.match`.
fn call_target(call: &str) -> &str {
    call.split_once('(').expect("a call").0
}

#[test]
fn find_takes_a_literal_and_returns_one_value() {
    assert_eq!(
        run("return string.find(\"hello\", \"ll\")"),
        LuaValue::Integer(3)
    );
    assert_eq!(run("return string.find(\"hello\", \"zz\")"), LuaValue::Nil);
    // Standard Lua returns (start, end); core's builtins give one value, so the
    // second name is nil rather than 4.
    let out = run("local a, b = string.find(\"hello\", \"ll\")\nreturn { a = a, b = b }");
    assert_eq!(field(&out, "a"), LuaValue::Integer(3));
    assert_eq!(field(&out, "b"), LuaValue::Nil);
}

#[test]
fn find_refuses_a_metacharacter_unless_the_plain_flag_is_true() {
    let refused = run(
        "local ok, err = pcall(function() return string.find(\"a.b\", \".\") end)\n\
         return { ok = ok, err = err }",
    );
    assert_eq!(field(&refused, "ok"), LuaValue::Boolean(false));
    assert_eq!(
        field(&refused, "err"),
        string("string patterns not supported; use literal string.find only")
    );
    assert_eq!(
        run("return string.find(\"a.b\", \".\", 1, true)"),
        LuaValue::Integer(2)
    );
    assert_eq!(
        run("return string.find_literal(\"a.b\", \".\")"),
        LuaValue::Integer(2)
    );
}

#[test]
fn the_colon_form_on_a_string_calls_the_string_module() {
    // Every `string` function the rules list, called both ways.
    for (method, by_name) in [
        ("s:len()", "string.len(s)"),
        ("s:sub(2, 3)", "string.sub(s, 2, 3)"),
        ("s:find(\"l\")", "string.find(s, \"l\")"),
        ("s:find(\".\", 1, true)", "string.find(s, \".\", 1, true)"),
        ("s:find_literal(\".\")", "string.find_literal(s, \".\")"),
        ("s:upper()", "string.upper(s)"),
        ("s:lower()", "string.lower(s)"),
        ("s:rep(2)", "string.rep(s, 2)"),
        ("s:byte(1)", "string.byte(s, 1)"),
        ("s:format()", "string.format(s)"),
    ] {
        let prelude = "local s = \"Hel.lo\"\n";
        let via_method = run(&format!("{prelude}return {method}"));
        assert_eq!(
            via_method,
            run(&format!("{prelude}return {by_name}")),
            "{method}"
        );
        assert_ne!(via_method, LuaValue::Nil, "{method}");
    }
    // `char` takes no string, so `s:char(66)` is `string.char(s, 66)` and
    // fails the same way.
    let out = run("local s = \"A\"\n\
         local ok, err = pcall(function() return s:char(66) end)\n\
         local ok2, err2 = pcall(function() return string.char(s, 66) end)\n\
         return { same = ok == ok2 and err == err2, ok = ok }");
    assert_eq!(field(&out, "same"), LuaValue::Boolean(true));
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(false));
    // A name the module lacks is an error that names it.
    let out = run(
        "local s = \"a\"\nlocal ok, err = pcall(function() return s:gfind(\"a\") end)\n\
         return { ok = ok, err = err }",
    );
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(false));
    assert_contains(&field(&out, "err"), "string.gfind does not exist");
}

#[test]
fn variadic_parameters_are_rejected() {
    assert_eq!(
        lint("local function f(...)\n  return 1\nend\nreturn f(1)").line,
        1
    );
}

#[test]
fn format_takes_d_s_x_and_percent_with_flags_width_and_precision() {
    assert_eq!(
        run("return string.format(\"%d %s %x %%\", 7, \"a\", 255)"),
        string("7 a ff %")
    );
    // The rules' own example, then `-`, `0`, width and precision on each.
    assert_eq!(run("return string.format(\"%05d\", 42)"), string("00042"));
    assert_eq!(
        run(
            "return string.format(\"%-5d|%5d|%.3d|%04x|%-4x|%6s|%-6s|%.2s\", \
             42, 42, 7, 255, 255, \"ab\", \"ab\", \"abcdef\")"
        ),
        string("42   |   42|007|00ff|ff  |    ab|ab    |ab")
    );
    for (spec, needle) in [
        ("%05s", "flag '0' is not valid with '%s'"),
        ("%100d", "at most 2 digits"),
    ] {
        let out = run(&format!(
            "local ok, err = pcall(function() return string.format(\"{spec}\", 1) end)\n\
             return {{ ok = ok, err = err }}"
        ));
        assert_eq!(field(&out, "ok"), LuaValue::Boolean(false), "{spec}");
        assert_contains(&field(&out, "err"), needle);
    }
}

#[test]
fn format_refuses_float_specifiers_and_points_at_decimal_format() {
    for spec in ["%f", "%.2f", "%e", "%g"] {
        let out = run(&format!(
            "local ok, err = pcall(function() return string.format(\"{spec}\", 1) end)\n\
             return {{ ok = ok, err = err }}"
        ));
        assert_eq!(field(&out, "ok"), LuaValue::Boolean(false), "{spec}");
        assert_contains(&field(&out, "err"), "no floats");
        assert_contains(&field(&out, "err"), "decimal.format");
    }
}

#[test]
fn a_function_returns_one_value_and_pcall_is_the_two_value_form() {
    assert_eq!(lint("return 1, 2").line, 1);
    assert_eq!(
        lint("local function f()\n  return 1, 2\nend\nreturn f()").line,
        2
    );
    let out =
        run("local ok, err = pcall(function() error(\"boom\") end)\nreturn { ok = ok, err = err }");
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(false));
    assert_eq!(field(&out, "err"), string("boom"));
    // A third name gets nil.
    let out = run("local ok, res, extra = pcall(function() return 5 end)\n\
         return { ok = ok, res = res, extra = extra }");
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(true));
    assert_eq!(field(&out, "res"), LuaValue::Integer(5));
    assert_eq!(field(&out, "extra"), LuaValue::Nil);
}

#[test]
fn pcall_in_a_single_value_position_gives_ok_alone() {
    let fails = "function() error(\"boom\") end";
    assert_eq!(
        run(&format!("local ok = pcall({fails})\nreturn ok")),
        LuaValue::Boolean(false)
    );
    assert_eq!(
        run(&format!("return pcall({fails})")),
        LuaValue::Boolean(false)
    );
    assert_eq!(
        run(&format!("if pcall({fails}) then return 1 end\nreturn 2")),
        LuaValue::Integer(2)
    );
    assert_eq!(
        run("return pcall(function() return 5 end)"),
        LuaValue::Boolean(true)
    );
}

#[test]
fn multiple_assignment_does_not_compile_and_names_the_local_form() {
    let err = lint("local ok, err\nok, err = pcall(function() return 5 end)\nreturn ok");
    assert_eq!(err.line, 2);
    assert!(
        err.message.contains("`local ok, err = pcall(...)`"),
        "{}",
        err.message
    );
}

#[test]
fn tonumber_refuses_a_decimal_string() {
    assert_eq!(run("return tonumber(\"2500\")"), LuaValue::Integer(2500));
    assert_eq!(run("return tonumber(\"2500.75\")"), LuaValue::Nil);
}

#[test]
fn decimal_parse_gives_scaled_integers_and_format_turns_them_back() {
    assert_eq!(
        run("return decimal.parse(\"2500.75\", 2)"),
        LuaValue::Integer(250075)
    );
    // Fewer fractional digits than the scale are padded.
    assert_eq!(
        run("return decimal.parse(\"2500.0\", 2)"),
        LuaValue::Integer(250000)
    );
    assert_eq!(run("return decimal.format(250075, 2)"), string("2500.75"));
    assert_eq!(
        run("return decimal.parse(\"2550.75\", 2) > decimal.parse(\"2500.0\", 2)"),
        LuaValue::Boolean(true)
    );
}

#[test]
fn decimal_parse_takes_only_a_string_so_tostring_covers_an_integer_field() {
    let out = run("return pcall(function() return decimal.parse(2500, 2) end)");
    assert_eq!(out, LuaValue::Boolean(false));
    assert_eq!(
        run("return decimal.parse(tostring(2500), 2)"),
        LuaValue::Integer(250000)
    );
    assert_eq!(
        run("return decimal.parse(tostring(\"2500.75\"), 2)"),
        LuaValue::Integer(250075)
    );
}

#[test]
fn decimal_parse_refuses_extra_digits_even_when_they_are_zeros() {
    let out = run(
        "local ok, err = pcall(function() return decimal.parse(\"2550.750\", 2) end)\n\
         return { ok = ok, err = err }",
    );
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(false));
    // The error points at the remedy the rules give.
    assert_contains(&field(&out, "err"), "decimal.rescale");
    assert_eq!(
        run("return decimal.rescale(decimal.parse(\"2550.750\", 3), 3, 2)"),
        LuaValue::Integer(255075)
    );
    // Narrowing past a non-zero digit fails rather than drop it.
    let out = run(
        "local ok = pcall(function() return decimal.rescale(255075, 2, 1) end)\n\
         return ok",
    );
    assert_eq!(out, LuaValue::Boolean(false));
}

#[test]
fn decimal_scales_run_from_0_to_18_and_exponents_are_refused() {
    for call in [
        "decimal.parse(\"1\", 18)",
        "decimal.parse(\"1\", 0)",
        "decimal.format(1, 18)",
    ] {
        let out = run(&format!("return pcall(function() return {call} end)"));
        assert_eq!(out, LuaValue::Boolean(true), "{call}");
    }
    for call in [
        "decimal.parse(\"1\", 19)",
        "decimal.format(1, -1)",
        "decimal.rescale(1, 0, 19)",
        "decimal.parse(\"1e3\", 2)",
    ] {
        let out = run(&format!("return pcall(function() return {call} end)"));
        assert_eq!(out, LuaValue::Boolean(false), "{call}");
    }
}

#[test]
fn a_tiny_tool_number_arrives_in_exponent_form_that_decimal_parse_refuses() {
    let response: serde_json::Value = serde_json::from_str(r#"{ "p": 0.0000001 }"#).unwrap();
    let table = proveno_gateway::values::json_to_table(&response).unwrap();
    let text = table
        .get(&proveno::types::table::LuaKey::String(LuaString::from_str(
            "p",
        )))
        .cloned()
        .unwrap();
    assert_eq!(text, string("1e-7"));
    let out = run("return pcall(function() return decimal.parse(\"1e-7\", 18) end)");
    assert_eq!(out, LuaValue::Boolean(false));
}

#[test]
fn return_inside_a_generic_for_fails_verification() {
    // A core compiler bug, present in v0.3.0 and v0.4.0. The rules warn about
    // it; when this test fails, core has fixed it, so remove the warning.
    for iterator in [
        "ipairs({ 1 })",
        "pairs({ a = 1 })",
        "pairs_sorted({ a = 1 })",
    ] {
        for program in [
            format!("for _, v in {iterator} do return false end\nreturn true"),
            format!("for _, v in {iterator} do if v then return false end end\nreturn true"),
            format!("for _, v in {iterator} do for i = 1, 2 do return i end end\nreturn true"),
            format!(
                "local function f()\n  for _, v in {iterator} do return false end\n  \
                 return true\nend\nreturn f()"
            ),
        ] {
            let err = lint(&program);
            assert_eq!(err.line, 0, "{program}");
            assert!(
                err.message
                    .starts_with("bytecode verification failed: RetStackMismatch"),
                "{program}: {}",
                err.message
            );
        }
    }
    // The alternatives the rules give.
    assert_eq!(
        run("local found = nil\n\
             for _, v in ipairs({ 1, 2 }) do\n\
               if v == 2 then found = v break end\n\
             end\n\
             return found"),
        LuaValue::Integer(2)
    );
    assert_eq!(
        run("local t = { 1, 2 }\n\
             for i = 1, #t do\n\
               if t[i] == 2 then return i end\n\
             end\n\
             return 0"),
        LuaValue::Integer(2)
    );
    // A function written in the loop body may return, so the pcall idiom works.
    assert_eq!(
        run("local n = 0\n\
             for _, v in ipairs({ 1, 2 }) do\n\
               local ok, r = pcall(function() return v end)\n\
               n = n + r\n\
             end\n\
             return n"),
        LuaValue::Integer(3)
    );
}

#[test]
fn a_function_statement_on_a_table_field_fails_verification() {
    // A core compiler bug, present in v0.3.0 and v0.4.0, whether or not the
    // function is called. When this test fails, remove the warning.
    for program in [
        "local t = {}\nfunction t:add(k) return k + 1 end\nreturn t:add(2)",
        "local t = {}\nfunction t.add(k) return k + 1 end\nreturn 1",
    ] {
        let err = lint(program);
        assert_eq!(err.line, 0, "{program}");
        assert!(
            err.message
                .starts_with("bytecode verification failed: RetStackMismatch"),
            "{program}: {}",
            err.message
        );
    }
    // The alternative the rules give.
    assert_eq!(
        run("local t = {}\nt.add = function(self, k) return k + 1 end\nreturn t:add(2)"),
        LuaValue::Integer(3)
    );
}

#[test]
fn every_global_in_the_rules_resolves() {
    // `pcall`, `error`, `log`, `print`, `pairs_sorted`, `pairs` and `ipairs`
    // are call-position only, so each is exercised by calling it.
    let out = run("local t = { b = 2, a = 1 }\n\
         local keys = {}\n\
         for k, _ in pairs_sorted(t) do table.insert(keys, k) end\n\
         for k, _ in pairs(t) do table.insert(keys, k) end\n\
         local items = {}\n\
         for _, v in ipairs({ 10, 20 }) do table.insert(items, v) end\n\
         log(\"a log line\")\n\
         print(\"a printed line\")\n\
         local ok, err = pcall(function() error(\"e\") end)\n\
         return {\n\
           keys = table.concat(keys, \",\"),\n\
           items = #items,\n\
           ok = ok,\n\
           err = err,\n\
           tostring = tostring(12),\n\
           tonumber = tonumber(\"12\"),\n\
           type = type(\"s\"),\n\
           select = select(2, \"a\", \"b\"),\n\
           unpack = unpack({ 5, 6 }),\n\
         }");
    assert_eq!(field(&out, "keys"), string("a,b,a,b"));
    assert_eq!(field(&out, "items"), LuaValue::Integer(2));
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(false));
    assert_eq!(field(&out, "err"), string("e"));
    assert_eq!(field(&out, "tostring"), string("12"));
    assert_eq!(field(&out, "tonumber"), LuaValue::Integer(12));
    assert_eq!(field(&out, "type"), string("string"));
    // The rules say so because standard Lua answers "number" here.
    assert_eq!(run("return type(7)"), string("integer"));
    assert_eq!(field(&out, "select"), string("b"));
    // One value per call, so `unpack` yields the first element only.
    assert_eq!(field(&out, "unpack"), LuaValue::Integer(5));
}

#[test]
fn json_decode_rejects_a_fraction_and_decode_strings_keeps_it() {
    assert_eq!(
        run("return json.decode(\"{\\\"n\\\":7}\").n"),
        LuaValue::Integer(7)
    );
    let out = run(
        "local ok, err = pcall(function() return json.decode(\"{\\\"n\\\":2.5}\") end)\n\
         return { ok = ok, err = err }",
    );
    assert_eq!(field(&out, "ok"), LuaValue::Boolean(false));
    assert_eq!(
        run("return json.decode_strings(\"{\\\"n\\\":2.5}\").n"),
        string("2.5")
    );
    assert_eq!(run("return json.encode({ n = 7 })"), string("{\"n\":7}"));
}

#[test]
fn integer_division_only() {
    assert_eq!(run("return 7 // 2"), LuaValue::Integer(3));
    assert_eq!(lint("return 7 / 2").line, 1);
    assert_eq!(
        lint("return 2.5").message,
        "floats are not supported; use integers or decimal strings"
    );
    assert_eq!(
        run("return math.scale_div(10, 3, 100)"),
        LuaValue::Integer(333)
    );
}

/// `math.scale_div`'s third argument is a multiplier, while `scale` in
/// `decimal.*` counts digits. A live model passed the digit count, 4, and got
/// a 2.03% move as 0; the rules must say which is which.
#[test]
fn scale_div_takes_a_multiplier_not_a_digit_count() {
    let program = |multiplier: &str| {
        format!(
            "local now = decimal.parse(\"2550.75\", 4)\n\
             local past = decimal.parse(\"2500.0\", 4)\n\
             return math.scale_div(now - past, past, {multiplier})"
        )
    };
    assert_eq!(run(&program("10000")), LuaValue::Integer(203));
    assert_eq!(run(&program("4")), LuaValue::Integer(0));
    let rules = dialect_rules();
    assert!(
        rules.contains("math.scale_div(now - past, past, 10000)"),
        "{rules}"
    );
    assert!(rules.contains("not a number of digits"), "{rules}");
}

fn assert_contains(v: &LuaValue, needle: &str) {
    match v {
        LuaValue::String(s) => {
            let text = String::from_utf8_lossy(s.as_bytes());
            assert!(
                text.contains(needle),
                "{text:?} does not contain {needle:?}"
            );
        }
        other => panic!("expected a string, got {other:?}"),
    }
}

fn field(v: &LuaValue, name: &str) -> LuaValue {
    match v {
        LuaValue::Table(t) => t
            .borrow()
            .get(&proveno::types::table::LuaKey::String(LuaString::from_str(
                name,
            )))
            .cloned()
            .unwrap_or(LuaValue::Nil),
        other => panic!("expected a table, got {other:?}"),
    }
}
