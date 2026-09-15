//! Mapping between JSON values and the VM's integer-only value model.

use std::cell::RefCell;
use std::rc::Rc;

use proveno::host::canonicalize::canonical_serialize_table;
use proveno::types::table::{LuaKey, LuaTable};
use proveno::types::value::{LuaString, LuaValue};
use serde_json::Value;

/// Program arguments to JSON, through the VM's one canonical encoding.
pub fn table_to_json(t: &LuaTable) -> Result<Value, String> {
    let bytes =
        canonical_serialize_table(t).map_err(|e| format!("cannot serialize arguments: {e:?}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("canonical arguments are not JSON: {e}"))
}

/// A downstream response to a table. The VM has no floats (spec section 8), so
/// any number that is not an `i64` becomes its decimal text.
pub fn json_to_table(v: &Value) -> Result<LuaTable, String> {
    match v {
        Value::Object(map) => object_to_table(map),
        other => Err(format!("response must be a JSON object, got {other}")),
    }
}

fn object_to_table(map: &serde_json::Map<String, Value>) -> Result<LuaTable, String> {
    let mut t = LuaTable::new();
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        set(&mut t, LuaKey::String(LuaString::from_str(key)), value)?;
    }
    Ok(t)
}

fn array_to_table(items: &[Value]) -> Result<LuaTable, String> {
    let mut t = LuaTable::new();
    for (i, value) in (1..).zip(items) {
        if value.is_null() {
            return Err(format!(
                "null at array index {i} cannot be represented in a table"
            ));
        }
        set(&mut t, LuaKey::Integer(i), value)?;
    }
    Ok(t)
}

fn set(t: &mut LuaTable, key: LuaKey, value: &Value) -> Result<(), String> {
    let value = to_lua(value)?;
    t.rawset(key, value)
        .map_err(|e| format!("cannot build response table: {e:?}"))
}

fn to_lua(v: &Value) -> Result<LuaValue, String> {
    Ok(match v {
        Value::Null => LuaValue::Nil,
        Value::Bool(b) => LuaValue::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => LuaValue::Integer(i),
            None => LuaValue::String(LuaString::from_str(&n.to_string())),
        },
        Value::String(s) => LuaValue::String(LuaString::from_str(s)),
        Value::Array(items) => table(array_to_table(items)?),
        Value::Object(map) => table(object_to_table(map)?),
    })
}

fn table(t: LuaTable) -> LuaValue {
    LuaValue::Table(Rc::new(RefCell::new(t)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn get(t: &LuaTable, key: &str) -> LuaValue {
        t.get(&LuaKey::String(LuaString::from_str(key)))
            .cloned()
            .unwrap_or(LuaValue::Nil)
    }

    fn string(v: &LuaValue) -> String {
        match v {
            LuaValue::String(s) => String::from_utf8(s.as_bytes().to_vec()).unwrap(),
            other => panic!("expected a string, got {other:?}"),
        }
    }

    #[test]
    fn float_becomes_decimal_string() {
        let t = json_to_table(&json!({ "price": 2500.5 })).unwrap();
        assert_eq!(string(&get(&t, "price")), "2500.5");
    }

    #[test]
    fn exponent_number_becomes_string() {
        let v: Value = serde_json::from_str(r#"{ "big": 1e3 }"#).unwrap();
        let t = json_to_table(&v).unwrap();
        assert_eq!(string(&get(&t, "big")), "1000.0");
    }

    #[test]
    fn i64_values_stay_integers() {
        let t = json_to_table(&json!({ "min": i64::MIN, "max": i64::MAX })).unwrap();
        assert!(matches!(get(&t, "min"), LuaValue::Integer(i64::MIN)));
        assert!(matches!(get(&t, "max"), LuaValue::Integer(i64::MAX)));
    }

    #[test]
    fn i64_overflow_becomes_string() {
        let v: Value = serde_json::from_str(r#"{ "n": 9223372036854775808 }"#).unwrap();
        let t = json_to_table(&v).unwrap();
        assert_eq!(string(&get(&t, "n")), "9223372036854775808");
    }

    #[test]
    fn null_member_is_omitted() {
        let t = json_to_table(&json!({ "a": 1, "b": null })).unwrap();
        assert!(t.get(&LuaKey::String(LuaString::from_str("b"))).is_none());
        assert_eq!(table_to_json(&t).unwrap(), json!({ "a": 1 }));
    }

    #[test]
    fn null_array_element_is_an_error() {
        let err = json_to_table(&json!({ "xs": [1, null, 3] })).unwrap_err();
        assert!(err.contains("index 2"), "{err}");
    }

    #[test]
    fn non_object_top_level_is_an_error() {
        for v in [json!([1, 2]), json!(3), json!("x"), json!(null)] {
            assert!(json_to_table(&v).is_err(), "{v}");
        }
    }

    #[test]
    fn nested_arrays_and_objects_survive() {
        let v = json!({
            "ok": true,
            "name": "wallet",
            "balances": [{ "asset": "ETH", "amount": 3 }, { "asset": "USDC", "amount": 20 }],
            "matrix": [[1, 2], [3, 4]],
            "meta": { "nested": { "deep": false } }
        });
        let t = json_to_table(&v).unwrap();
        assert_eq!(table_to_json(&t).unwrap(), v);
    }

    #[test]
    fn array_keys_start_at_one() {
        let t = json_to_table(&json!({ "xs": ["a", "b"] })).unwrap();
        let LuaValue::Table(xs) = get(&t, "xs") else {
            panic!("expected a table");
        };
        let xs = xs.borrow();
        assert_eq!(string(xs.get(&LuaKey::Integer(1)).unwrap()), "a");
        assert_eq!(string(xs.get(&LuaKey::Integer(2)).unwrap()), "b");
        assert_eq!(xs.length(), 2);
    }

    #[test]
    fn canonical_bytes_are_stable_across_runs() {
        let v: Value = serde_json::from_str(
            r#"{ "z": 1, "a": [2.5, { "y": "s", "b": 9223372036854775808 }], "m": null }"#,
        )
        .unwrap();
        let first = canonical_serialize_table(&json_to_table(&v).unwrap()).unwrap();
        let second = canonical_serialize_table(&json_to_table(&v).unwrap()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            String::from_utf8(first).unwrap(),
            r#"{"a":["2.5",{"b":"9223372036854775808","y":"s"}],"z":1}"#
        );
    }

    #[test]
    fn table_to_json_uses_canonical_encoding() {
        let mut t = LuaTable::new();
        t.rawset(
            LuaKey::String(LuaString::from_str("to")),
            LuaValue::String(LuaString::from_str("0x1")),
        )
        .unwrap();
        t.rawset(
            LuaKey::String(LuaString::from_str("amount")),
            LuaValue::Integer(20),
        )
        .unwrap();
        assert_eq!(
            table_to_json(&t).unwrap(),
            json!({ "to": "0x1", "amount": 20 })
        );
    }
}
