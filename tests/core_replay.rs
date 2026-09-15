//! Pins the proveno-core property the gateway is built on: a run replayed from
//! its recorded tape is identical to the live run, under the feature set the
//! gateway depends on.

use proveno::{
    HostInterface, OracleTape, TapeHost, Vm, VmConfig,
    bytecode::verify,
    compiler::{compile, program_hash::compute_program_hash_sha256},
    parser::parse,
    types::{
        table::{LuaKey, LuaTable},
        value::{LuaString, LuaValue},
    },
};

struct PriceHost;

impl HostInterface for PriceHost {
    fn call_tool(&mut self, name: &str, _args: &LuaTable) -> Result<LuaTable, String> {
        if name != "market.get_price" {
            return Err(format!("denied: {name}"));
        }
        let mut t = LuaTable::new();
        t.rawset(
            LuaKey::String(LuaString::from_str("price")),
            LuaValue::Integer(2500),
        )
        .unwrap();
        Ok(t)
    }
}

const PROGRAM: &str = r#"
    local p = tool.call("market.get_price", {pair = "ETH/USD"})
    local ok, err = pcall(function()
        return tool.call("wallet.transfer", {amount = 20})
    end)
    if ok then
        return p.price
    end
    return p.price * 2
"#;

#[test]
fn replay_from_tape_matches_live_run() {
    let program = compile(&parse(PROGRAM).unwrap()).unwrap();
    verify(&program).unwrap();

    let live = Vm::new(VmConfig::default(), PriceHost)
        .execute(&program, LuaValue::Nil)
        .unwrap();
    assert_eq!(live.return_value, LuaValue::Integer(5000));
    assert_eq!(live.transcript.len(), 2);

    let tape = OracleTape::from_records(&live.transcript);
    let replayed = Vm::new(VmConfig::default(), TapeHost::new(tape))
        .execute(&program, LuaValue::Nil)
        .unwrap();

    assert_eq!(replayed.return_value, live.return_value);
    assert_eq!(replayed.gas_used, live.gas_used);
    assert_eq!(replayed.memory_used, live.memory_used);
}

#[test]
fn sha256_program_hash_covers_constants() {
    let hash =
        |src: &str| compute_program_hash_sha256(&compile(&parse(src).unwrap()).unwrap().prototypes);
    assert_ne!(hash("return 1"), hash("return 2"));
}
