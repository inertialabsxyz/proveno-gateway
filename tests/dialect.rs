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
const LIBRARY: [(&str, &str, &str); 24] = [
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
    // Standard Lua has all four. Core has none of them, which is why the rules
    // list members rather than `string.*`.
    for absent in ["math.floor", "math.fmod", "string.gfind", "table.unpack"] {
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
    assert_eq!(
        run("local s = \"hello\"\nreturn s:sub(1, 2)"),
        run("local s = \"hello\"\nreturn string.sub(s, 1, 2)")
    );
    assert_eq!(run("local s = \"hello\"\nreturn s:sub(1, 2)"), string("he"));
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
    assert_eq!(run("return string.format(\"%5d\", 7)"), string("    7"));
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
}

#[test]
fn tonumber_refuses_a_decimal_string_and_the_documented_split_works() {
    assert_eq!(run("return tonumber(\"2500\")"), LuaValue::Integer(2500));
    assert_eq!(run("return tonumber(\"2500.75\")"), LuaValue::Nil);
    // The idiom the rules give: find the dot, cut, scale to hundredths.
    // Step 7 replaces this with `decimal.parse`.
    assert_eq!(
        run("local s = \"2500.75\"\n\
             local dot = string.find_literal(s, \".\")\n\
             local whole = tonumber(string.sub(s, 1, dot - 1))\n\
             local frac = tonumber(string.sub(s, dot + 1))\n\
             return whole * 100 + frac"),
        LuaValue::Integer(250075)
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
