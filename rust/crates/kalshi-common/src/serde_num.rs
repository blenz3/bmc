//! Tolerant numeric serde helpers for Kalshi wire fields.
//!
//! Kalshi's production wire serializes most `_dollars` and `_fp` fields as JSON
//! strings (`"0.5500"`, `"33413.00"`), but the docs and some endpoints still
//! ship plain numbers. These helpers accept either form. Apply with
//! `#[serde(deserialize_with = "serde_num::as_f64")]` etc. on the field;
//! serialization continues to use serde's default (writes a number).
//!
//! `_fp` integer fields can land as `"2.00"` on the wire — string with a
//! decimal point. The `as_i64*` helpers parse via `f64` and truncate so those
//! parse cleanly.

use serde::{de::Error as DeError, Deserialize, Deserializer};

#[derive(Deserialize)]
#[serde(untagged)]
pub enum NumOrStr {
    Num(f64),
    Str(String),
}

pub fn to_f64<E: DeError>(v: NumOrStr) -> Result<f64, E> {
    match v {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => s.parse::<f64>().map_err(E::custom),
    }
}

pub fn to_i64<E: DeError>(v: NumOrStr) -> Result<i64, E> {
    match v {
        NumOrStr::Num(n) => Ok(n as i64),
        // Accept "33413.00" or similar by parsing as f64 then truncating.
        NumOrStr::Str(s) => s
            .parse::<f64>()
            .map(|f| f as i64)
            .map_err(E::custom),
    }
}

pub fn as_f64<'de, D>(d: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    to_f64::<D::Error>(NumOrStr::deserialize(d)?)
}

pub fn as_f64_opt<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<NumOrStr>::deserialize(d)?
        .map(to_f64::<D::Error>)
        .transpose()
}

pub fn as_i64<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    to_i64::<D::Error>(NumOrStr::deserialize(d)?)
}

pub fn as_i64_opt<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<NumOrStr>::deserialize(d)?
        .map(to_i64::<D::Error>)
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize)]
    struct F {
        #[serde(deserialize_with = "as_f64")]
        v: f64,
    }

    #[derive(Deserialize)]
    struct I {
        #[serde(deserialize_with = "as_i64")]
        v: i64,
    }

    #[test]
    fn f64_accepts_number() {
        let f: F = serde_json::from_value(json!({ "v": 0.55 })).unwrap();
        assert!((f.v - 0.55).abs() < 1e-9);
    }

    #[test]
    fn f64_accepts_string() {
        let f: F = serde_json::from_value(json!({ "v": "0.5500" })).unwrap();
        assert!((f.v - 0.55).abs() < 1e-9);
    }

    #[test]
    fn i64_accepts_number_and_string() {
        let i: I = serde_json::from_value(json!({ "v": 33413 })).unwrap();
        assert_eq!(i.v, 33413);
        let i: I = serde_json::from_value(json!({ "v": "33413.00" })).unwrap();
        assert_eq!(i.v, 33413);
        let i: I = serde_json::from_value(json!({ "v": "-2.5" })).unwrap();
        assert_eq!(i.v, -2);
    }
}
