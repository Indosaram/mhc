//! Port of omo-senpi `components/x-search/params.ts` at latest `455dee62`.

use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::format::XSearchErrorCode;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XSearchParams {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_x_handles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_x_handles: Option<Vec<String>>,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub enable_image_understanding: bool,
    #[serde(default)]
    pub enable_video_understanding: bool,
}

fn default_mode() -> String {
    "latest".to_string()
}

fn default_max_results() -> usize {
    10
}

pub fn x_search_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1, "maxLength": 2000 },
            "from_date": { "type": "string", "pattern": r"^\d{4}-\d{2}-\d{2}$" },
            "to_date": { "type": "string", "pattern": r"^\d{4}-\d{2}-\d{2}$" },
            "allowed_x_handles": {
                "type": "array",
                "items": { "type": "string", "pattern": r"^[A-Za-z0-9_]{1,15}$" },
                "minItems": 1,
                "maxItems": 20,
                "uniqueItems": true
            },
            "excluded_x_handles": {
                "type": "array",
                "items": { "type": "string", "pattern": r"^[A-Za-z0-9_]{1,15}$" },
                "minItems": 1,
                "maxItems": 20,
                "uniqueItems": true
            },
            "mode": { "type": "string", "enum": ["latest", "top"], "default": "latest" },
            "max_results": { "type": "integer", "minimum": 1, "maximum": 30, "default": 10 },
            "enable_image_understanding": { "type": "boolean", "default": false },
            "enable_video_understanding": { "type": "boolean", "default": false }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XSearchValidation {
    Ok(XSearchParams),
    Err { code: XSearchErrorCode, message: String },
}

fn failure(code: XSearchErrorCode, message: &str) -> XSearchValidation {
    XSearchValidation::Err {
        code,
        message: message.to_string(),
    }
}

/// Upstream validDate:
/// 1. matches /^\d{4}-\d{2}-\d{2}$/
/// 2. [year, month, day] = value.split("-").map(Number)
/// 3. const date = new Date(Date.UTC(year, month - 1, day))
/// 4. return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day
///
/// Note on Date.UTC behavior: year 0..99 maps to 1900..1999 in JS Date.UTC, so year < 100 never matches year!
fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        if i == 4 || i == 7 {
            continue;
        }
        if !b.is_ascii_digit() {
            return false;
        }
    }

    let year = value[0..4].parse::<i32>().unwrap_or(0);
    let month = value[5..7].parse::<u32>().unwrap_or(0);
    let day = value[8..10].parse::<u32>().unwrap_or(0);

    // In JS Date.UTC(year, month - 1, day), years 0000..0099 map to 1900..1999,
    // so date.getUTCFullYear() === year evaluates to false for years 0..99.
    if year < 100 {
        return false;
    }

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }

    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let max_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    day <= max_days
}

fn check_schema(input: &serde_json::Map<String, Value>) -> bool {
    const ALLOWED: &[&str] = &[
        "query",
        "from_date",
        "to_date",
        "allowed_x_handles",
        "excluded_x_handles",
        "mode",
        "max_results",
        "enable_image_understanding",
        "enable_video_understanding",
    ];
    for key in input.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return false;
        }
    }

    // query: string, minLength 1, maxLength 2000 (UTF-16 code units in JS TypeBox)
    let Some(q_val) = input.get("query") else { return false };
    let Some(q_str) = q_val.as_str() else { return false };
    let utf16_len = q_str.encode_utf16().count();
    if !(1..=2000).contains(&utf16_len) {
        return false;
    }

    let is_handle = |s: &str| {
        if s.is_empty() || s.len() > 15 {
            return false;
        }
        s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };

    // allowed_x_handles
    if let Some(val) = input.get("allowed_x_handles") {
        let Some(arr) = val.as_array() else { return false };
        if arr.is_empty() || arr.len() > 20 {
            return false;
        }
        let mut seen = HashSet::new();
        for item in arr {
            let Some(s) = item.as_str() else { return false };
            if !is_handle(s) || !seen.insert(s) {
                return false;
            }
        }
    }

    // excluded_x_handles
    if let Some(val) = input.get("excluded_x_handles") {
        let Some(arr) = val.as_array() else { return false };
        if arr.is_empty() || arr.len() > 20 {
            return false;
        }
        let mut seen = HashSet::new();
        for item in arr {
            let Some(s) = item.as_str() else { return false };
            if !is_handle(s) || !seen.insert(s) {
                return false;
            }
        }
    }

    // mode
    if let Some(val) = input.get("mode") {
        let Some(s) = val.as_str() else { return false };
        if s != "latest" && s != "top" {
            return false;
        }
    }

    // max_results
    if let Some(val) = input.get("max_results") {
        let Some(n) = val.as_i64() else { return false };
        if !(1..=30).contains(&n) {
            return false;
        }
    }

    // enable_image_understanding
    if let Some(val) = input.get("enable_image_understanding")
        && !val.is_boolean()
    {
        return false;
    }

    // enable_video_understanding
    if let Some(val) = input.get("enable_video_understanding")
        && !val.is_boolean()
    {
        return false;
    }

    true
}

/// Upstream exact validateXSearchParams:
/// 1. typeof raw !== "object" || raw === null || Array.isArray(raw) => INVALID_PARAMS, "Parameters must be an object"
/// 2. typeof input.query === "string" && input.query.trim().length === 0 => INVALID_PARAMS, "query must not be blank"
/// 3. input.allowed_x_handles !== undefined && input.excluded_x_handles !== undefined => INVALID_FILTERS, "allowed and excluded handles are mutually exclusive"
/// 4. for key in ["allowed_x_handles", "excluded_x_handles"]: Array.isArray(handles) && handles.length > 20 => TOO_MANY_HANDLES, "{key} cannot contain more than 20 handles"
/// 5. for key in ["from_date", "to_date"]: input[key] !== undefined && (typeof input[key] !== "string" || !validDate(input[key])) => INVALID_DATE, "{key} must be a valid ISO calendar date"
/// 6. typeof input.from_date === "string" && typeof input.to_date === "string" && input.from_date > input.to_date => INVALID_DATE_RANGE, "from_date must be on or before to_date"
/// 7. !Value.Check(XSearchParams, raw) => INVALID_PARAMS, "Parameters failed validation"
pub fn validate_x_search_params(raw: &Value) -> XSearchValidation {
    let Some(input) = raw.as_object() else {
        return failure(XSearchErrorCode::InvalidParams, "Parameters must be an object");
    };

    if let Some(query_str) = input.get("query").and_then(Value::as_str)
        && query_str.trim().is_empty()
    {
        return failure(XSearchErrorCode::InvalidParams, "query must not be blank");
    }

    if input.get("allowed_x_handles").is_some() && input.get("excluded_x_handles").is_some() {
        return failure(
            XSearchErrorCode::InvalidFilters,
            "allowed and excluded handles are mutually exclusive",
        );
    }

    for key in ["allowed_x_handles", "excluded_x_handles"] {
        if let Some(handles_arr) = input.get(key).and_then(Value::as_array)
            && handles_arr.len() > 20
        {
            return failure(
                XSearchErrorCode::TooManyHandles,
                &format!("{key} cannot contain more than 20 handles"),
            );
        }
    }

    for key in ["from_date", "to_date"] {
        if let Some(val) = input.get(key) {
            let is_valid = val.as_str().is_some_and(valid_date);
            if !is_valid {
                return failure(
                    XSearchErrorCode::InvalidDate,
                    &format!("{key} must be a valid ISO calendar date"),
                );
            }
        }
    }

    if let (Some(from), Some(to)) = (
        input.get("from_date").and_then(Value::as_str),
        input.get("to_date").and_then(Value::as_str),
    ) && from > to
    {
        return failure(
            XSearchErrorCode::InvalidDateRange,
            "from_date must be on or before to_date",
        );
    }

    if !check_schema(input) {
        return failure(XSearchErrorCode::InvalidParams, "Parameters failed validation");
    }

    let query = input.get("query").and_then(Value::as_str).unwrap_or("").to_string();
    let from_date = input.get("from_date").and_then(Value::as_str).map(str::to_string);
    let to_date = input.get("to_date").and_then(Value::as_str).map(str::to_string);
    let allowed_x_handles = input.get("allowed_x_handles").and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(Value::as_str).map(str::to_string).collect()
    });
    let excluded_x_handles = input.get("excluded_x_handles").and_then(Value::as_array).map(|arr| {
        arr.iter().filter_map(Value::as_str).map(str::to_string).collect()
    });
    let mode = input.get("mode").and_then(Value::as_str).unwrap_or("latest").to_string();
    let max_results = input.get("max_results").and_then(Value::as_u64).unwrap_or(10) as usize;
    let enable_image_understanding = input.get("enable_image_understanding").and_then(Value::as_bool).unwrap_or(false);
    let enable_video_understanding = input.get("enable_video_understanding").and_then(Value::as_bool).unwrap_or(false);

    XSearchValidation::Ok(XSearchParams {
        query,
        from_date,
        to_date,
        allowed_x_handles,
        excluded_x_handles,
        mode,
        max_results,
        enable_image_understanding,
        enable_video_understanding,
    })
}
