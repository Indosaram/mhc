//! `store/provisional-exit-parse.ts`: the optional `provisional_exit` object of a stored record.

use serde_json::{Map, Value};

use crate::state::ProvisionalExit;

type Object = Map<String, Value>;

pub fn parse_optional_provisional_exit(record: &Object) -> Result<Option<ProvisionalExit>, String> {
    let Some(value) = record.get("provisional_exit") else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or("provisional_exit is not an object")?;
    Ok(Some(ProvisionalExit {
        observed_at: read_string(object, "observed_at")?,
        run_epoch: read_integer(object, "run_epoch")?,
        code: match object.get("code") {
            Some(Value::Null) => None,
            _ => Some(read_integer(object, "code")?),
        },
        signal: match object.get("signal") {
            Some(Value::Null) => None,
            _ => Some(read_string(object, "signal")?),
        },
    }))
}

fn read_string(object: &Object, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is not a string"))
}

fn read_integer(object: &Object, key: &str) -> Result<i64, String> {
    object
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.fract() == 0.0)
        .map(|number| number as i64)
        .ok_or_else(|| format!("{key} is not a number"))
}
