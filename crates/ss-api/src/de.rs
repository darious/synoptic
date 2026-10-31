//! Lenient deserialisation helpers.
//!
//! Synology sends numbers as strings in places (`"id": "42"`, `"major": "9"`),
//! booleans as numbers, and leaves fields out entirely. The reference client
//! papered over that with Python's `int()` / `bool()` / `dict.get(..., 0)`;
//! these helpers do the same for the `serde` models: a missing field is its
//! default, a numeric string is parsed, anything unparsable is the default.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// `Value` → integer the way Python's `int()` would, with 0 for nonsense.
pub(crate) fn coerce_int(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Value::Bool(b) => i64::from(*b),
        Value::String(s) => {
            let s = s.trim();
            s.parse::<i64>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
                .unwrap_or(0)
        }
        _ => 0,
    }
}

/// `Value` → bool: real booleans, non-zero numbers and the usual true-ish
/// strings. (Unlike Python's `bool("false")`, the string `"false"` is false.)
pub(crate) fn coerce_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

/// `Value` → string: strings as-is, scalars stringified, null → `""`.
pub(crate) fn coerce_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn int<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(coerce_int(&Value::deserialize(d)?))
}

pub(crate) fn opt_int<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Null => None,
        other => Some(coerce_int(&other)),
    })
}

pub(crate) fn boolish<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(coerce_bool(&Value::deserialize(d)?))
}

pub(crate) fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(coerce_string(&Value::deserialize(d)?))
}

/// Decode a `"1,2,3"` list the way Event Center's `Capability` sends it.
pub(crate) fn csv_ints<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i64>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Array(items) => items.iter().map(coerce_int).collect(),
        Value::String(s) => s
            .split(',')
            .filter_map(|p| p.trim().parse::<i64>().ok())
            .collect(),
        Value::Number(_) => vec![coerce_int(&v)],
        _ => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ints() {
        assert_eq!(coerce_int(&json!(7)), 7);
        assert_eq!(coerce_int(&json!("42")), 42);
        assert_eq!(coerce_int(&json!(" 3 ")), 3);
        assert_eq!(coerce_int(&json!("1.0")), 1);
        assert_eq!(coerce_int(&json!(2.9)), 2);
        assert_eq!(coerce_int(&json!(true)), 1);
        assert_eq!(coerce_int(&json!("motion")), 0);
        assert_eq!(coerce_int(&json!(null)), 0);
    }

    #[test]
    fn bools() {
        assert!(coerce_bool(&json!(true)));
        assert!(coerce_bool(&json!(2)));
        assert!(coerce_bool(&json!("true")));
        assert!(coerce_bool(&json!("1")));
        assert!(!coerce_bool(&json!("false")));
        assert!(!coerce_bool(&json!(0)));
        assert!(!coerce_bool(&json!(null)));
    }

    #[test]
    fn csv() {
        #[derive(Deserialize)]
        struct T {
            #[serde(default, deserialize_with = "csv_ints")]
            v: Vec<i64>,
        }
        let t: T = serde_json::from_value(json!({"v": "1, 2,x,4"})).unwrap();
        assert_eq!(t.v, vec![1, 2, 4]);
        let t: T = serde_json::from_value(json!({"v": [5, "6"]})).unwrap();
        assert_eq!(t.v, vec![5, 6]);
        let t: T = serde_json::from_value(json!({})).unwrap();
        assert!(t.v.is_empty());
    }
}
