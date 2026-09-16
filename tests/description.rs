//! The generated prelude and description against a real proveno-core compile
//! and VM run, and the pre-execution lint through `compile_program`.

use std::{cell::RefCell, rc::Rc};

use proveno::{
    HostInterface, Vm, VmConfig,
    types::{
        table::{LuaKey, LuaTable},
        value::{LuaString, LuaValue},
    },
};
use proveno_gateway::{
    description::{EXAMPLE_TOOL, EXAMPLES, build},
    dialect::{compile_program, lua_guide},
    downstream::ToolSchema,
};
use serde_json::json;

fn tool(server: &str, name: &str) -> ToolSchema {
    ToolSchema {
        server: server.into(),
        name: name.into(),
        description: format!("The {name} tool"),
        input_schema: json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        }),
        output_schema: Some(json!({
            "type": "object",
            "properties": { "status": { "type": "string" } }
        })),
    }
}

fn example_tool() -> ToolSchema {
    tool(EXAMPLE_TOOL.0, EXAMPLE_TOOL.1)
}

/// Records every call; denies names in `deny`, answers the rest with
/// `{ status = "ok", price = "2550.75", id = <args.id> }`.
struct MockHost {
    calls: Rc<RefCell<Vec<String>>>,
    deny: Vec<&'static str>,
}

impl MockHost {
    fn new(deny: Vec<&'static str>) -> Self {
        Self {
            calls: Rc::default(),
            deny,
        }
    }
}

fn key(s: &str) -> LuaKey {
    LuaKey::String(LuaString::from_str(s))
}

impl HostInterface for MockHost {
    fn call_tool(&mut self, name: &str, args: &LuaTable) -> Result<LuaTable, String> {
        self.calls.borrow_mut().push(name.to_string());
        if self.deny.contains(&name) {
            return Err(format!("denied: {name}"));
        }
        let mut t = LuaTable::new();
        t.rawset(key("status"), LuaValue::String(LuaString::from_str("ok")))
            .unwrap();
        t.rawset(
            key("price"),
            LuaValue::String(LuaString::from_str("2550.75")),
        )
        .unwrap();
        if let Some(id) = args.get(&key("id")) {
            t.rawset(key("id"), id.clone()).unwrap();
        }
        Ok(t)
    }
}

fn field(v: &LuaValue, name: &str) -> LuaValue {
    match v {
        LuaValue::Table(t) => t
            .borrow()
            .get(&key(name))
            .cloned()
            .unwrap_or_else(|| panic!("missing field {name}")),
        other => panic!("expected table, got {other:?}"),
    }
}

#[test]
fn description_is_pure_under_shuffling() {
    let tools = vec![tool("b", "two"), tool("a", "one"), tool("b", "one")];
    let mut shuffled = tools.clone();
    shuffled.swap(0, 2);
    assert_eq!(build(&tools), build(&shuffled));
}

#[test]
fn every_example_in_the_description_compiles() {
    let desc = build(&[example_tool(), tool("other", "thing")]);
    for example in EXAMPLES {
        assert!(desc.text.contains(example), "example missing from text");
        compile_program(&desc.prelude, example)
            .unwrap_or_else(|e| panic!("example failed to compile: {e}\n{example}"));
    }
    for example in EXAMPLES {
        assert!(lua_guide().contains(example), "example missing from guide");
    }
}

#[test]
fn every_example_runs_against_its_placeholder_tool() {
    let desc = build(&[example_tool()]);
    for example in EXAMPLES {
        let program = compile_program(&desc.prelude, example).unwrap();
        let mut vm = Vm::new(VmConfig::default(), MockHost::new(vec![]));
        vm.execute(&program, LuaValue::Nil)
            .unwrap_or_else(|e| panic!("example failed at runtime: {e:?}\n{example}"));
    }
}

#[test]
fn prelude_function_reaches_the_host_with_the_qualified_name() {
    let desc = build(&[tool("wallet", "transfer"), tool("market", "get_price")]);
    let program = compile_program(
        &desc.prelude,
        "local r = market.get_price{ id = \"ETH\" }\nreturn r.id",
    )
    .unwrap();
    let host = MockHost::new(vec![]);
    let calls = Rc::clone(&host.calls);
    let out = Vm::new(VmConfig::default(), host)
        .execute(&program, LuaValue::Nil)
        .unwrap();
    assert_eq!(
        out.return_value,
        LuaValue::String(LuaString::from_str("ETH"))
    );
    assert_eq!(*calls.borrow(), vec!["market.get_price"]);
}

#[test]
fn denied_call_is_catchable_as_in_the_example() {
    let desc = build(&[example_tool()]);
    let program = compile_program(&desc.prelude, EXAMPLES[1]).unwrap();
    let out = Vm::new(VmConfig::default(), MockHost::new(vec!["example.lookup"]))
        .execute(&program, LuaValue::Nil)
        .unwrap();
    assert_eq!(field(&out.return_value, "ok"), LuaValue::Boolean(false));
}

#[test]
fn decimal_example_parses_compares_and_formats_the_price() {
    let desc = build(&[example_tool()]);
    let program = compile_program(&desc.prelude, EXAMPLES[3]).unwrap();
    let out = Vm::new(VmConfig::default(), MockHost::new(vec![]))
        .execute(&program, LuaValue::Nil)
        .unwrap();
    // 2550.75 is above the 2500.00 limit.
    assert_eq!(field(&out.return_value, "buy"), LuaValue::Boolean(false));
    assert_eq!(
        field(&out.return_value, "price"),
        LuaValue::String(LuaString::from_str("2550.75"))
    );
}

#[test]
fn lint_os_time_reports_program_line_4() {
    let desc = build(&[example_tool()]);
    let program = "local a = 1\nlocal b = 2\nlocal c = 3\nlocal t = os.time()\nreturn t";
    let err = compile_program(&desc.prelude, program).unwrap_err();
    assert_eq!(err.line, 4);
    assert_eq!(
        err.to_string(),
        "line 4: `os` is not available; time and randomness are tool calls"
    );
}

#[test]
fn lint_float_literal() {
    let desc = build(&[example_tool()]);
    let err = compile_program(&desc.prelude, "local x = 1\nreturn 2.5").unwrap_err();
    assert_eq!(err.line, 2);
    assert_eq!(
        err.message,
        "floats are not supported; use integers or decimal strings"
    );
}

#[test]
fn lint_undefined_global() {
    let desc = build(&[example_tool()]);
    let err = compile_program(&desc.prelude, "total = 5\nreturn total").unwrap_err();
    assert_eq!(err.line, 1);
    assert_eq!(
        err.message,
        "`total` is not defined; declare it `local`, or check the tool API"
    );
}

#[test]
fn lint_denied_tool_namespace_is_undefined() {
    let desc = build(&[example_tool()]);
    let err = compile_program(&desc.prelude, "return wallet.transfer{ id = \"x\" }").unwrap_err();
    assert_eq!(err.line, 1);
    assert!(err.message.starts_with("`wallet` is not defined"));
}

#[test]
fn lint_pcall_tool_call() {
    let desc = build(&[example_tool()]);
    let program = "local ok, r = pcall(tool.call, \"example.lookup\", { id = \"a\" })\nreturn ok";
    let err = compile_program(&desc.prelude, program).unwrap_err();
    assert_eq!(err.line, 1);
    assert!(
        err.message
            .contains("use `pcall(function() return tool.call(name, args) end)`"),
        "{}",
        err.message
    );
}

#[test]
fn lint_other_disallowed_identifiers_and_require() {
    let desc = build(&[example_tool()]);
    let err = compile_program(&desc.prelude, "local m = require(\"x\")").unwrap_err();
    assert_eq!(
        err.message,
        "`require` is not available; time and randomness are tool calls"
    );
    let err = compile_program(&desc.prelude, "\nlocal m = coroutine").unwrap_err();
    assert_eq!(err.line, 2);
    assert!(err.message.contains("`coroutine`"), "{}", err.message);
}
