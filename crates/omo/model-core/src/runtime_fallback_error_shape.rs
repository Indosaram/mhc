use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::js_value::get;
use crate::js_value::is_truthy;
use crate::js_value::json_stringify;

const DEFAULT_RETRY_ON_ERRORS: [i64; 5] = [429, 500, 502, 503, 504];

fn nested_record<'a>(record: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    get(record, key).filter(|value| value.is_object() || value.is_array())
}

fn non_empty_string<'a>(record: &'a Value, key: &str) -> Option<&'a str> {
    get(Some(record), key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// Lowercased human-readable message of an arbitrary provider error payload.
#[must_use]
#[expect(clippy::expect_used, reason = "static regex literal is valid")]
pub fn get_runtime_fallback_error_message(error: Option<&Value>) -> String {
    static NAME_COLON: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r":\s*(.+)").expect("valid regex"));

    let Some(error) = error.filter(|error| is_truthy(error)) else {
        return String::new();
    };
    if let Value::String(text) = error {
        return text.to_lowercase();
    }
    if !(error.is_object() || error.is_array()) {
        return json_stringify(error).to_lowercase();
    }

    let data = nested_record(Some(error), "data");
    let nested_error = nested_record(Some(error), "error");
    let data_error = nested_record(data, "error");
    let records = [data, nested_error, Some(error), data_error];
    if let Some(message) = records
        .iter()
        .flatten()
        .find_map(|record| non_empty_string(record, "message"))
    {
        return message.to_lowercase();
    }

    if let Some(name) = non_empty_string(error, "name")
        && let Some(captures) = NAME_COLON.captures(name)
    {
        return captures
            .get(1)
            .map_or(String::new(), |m| m.as_str().trim().to_lowercase());
    }

    json_stringify(error).to_lowercase()
}

/// HTTP status from `statusCode`/`status`, nested records, or a retry code in the message.
#[must_use]
pub fn get_runtime_fallback_status_code(
    error: Option<&Value>,
    retry_on_errors: Option<&[i64]>,
) -> Option<i64> {
    if let Some(status) = get(error, "statusCode")
        .and_then(Value::as_i64)
        .or_else(|| get(error, "status").and_then(Value::as_i64))
    {
        return Some(status);
    }

    let root = error.filter(|error| error.is_object() || error.is_array());
    let nested = [
        nested_record(root, "data"),
        nested_record(root, "error"),
        nested_record(root, "cause"),
    ];
    if let Some(status) = nested
        .iter()
        .find_map(|record| get(*record, "statusCode").and_then(Value::as_i64))
    {
        return Some(status);
    }

    let retry_codes = retry_on_errors.unwrap_or(&DEFAULT_RETRY_ON_ERRORS);
    if retry_codes.is_empty() {
        return None;
    }
    let alternatives = retry_codes
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join("|");
    let pattern = Regex::new(&format!(r"(?-u:\b)({alternatives})(?-u:\b)")).ok()?;
    let message = get_runtime_fallback_error_message(error);
    pattern.captures(&message)?.get(1)?.as_str().parse().ok()
}

/// First non-empty `name` on the error or its nested `data`/`error` records.
#[must_use]
pub fn get_runtime_fallback_error_name(error: Option<&Value>) -> Option<String> {
    let error = error.filter(|error| error.is_object() || error.is_array())?;
    let data = nested_record(Some(error), "data");
    let records = [
        Some(error),
        data,
        nested_record(Some(error), "error"),
        nested_record(data, "error"),
    ];
    records
        .iter()
        .flatten()
        .find_map(|record| non_empty_string(record, "name"))
        .map(str::to_string)
}

/// The AI SDK `isRetryable` flag, if any record carries a boolean one.
#[must_use]
pub fn get_runtime_fallback_retryable_signal(error: Option<&Value>) -> Option<bool> {
    let error = error.filter(|error| error.is_object() || error.is_array())?;
    let data = nested_record(Some(error), "data");
    let records = [
        Some(error),
        data,
        nested_record(Some(error), "error"),
        nested_record(data, "error"),
        nested_record(Some(error), "cause"),
    ];
    records
        .iter()
        .flatten()
        .find_map(|record| get(Some(record), "isRetryable").and_then(Value::as_bool))
}
