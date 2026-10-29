//! Parameter schema (spec §2) and validation (spec §3).

use crate::{ErrorCode, ToolFailure};
use serde_json::{Map, Value, json};

pub const QUERY_MAX_UNITS: usize = 2000;
pub const HANDLES_MAX: usize = 20;
pub const MAX_RESULTS_DEFAULT: u64 = 10;
pub const MAX_RESULTS_LIMIT: u64 = 30;
const HANDLE_KEYS: [&str; 2] = ["allowed_x_handles", "excluded_x_handles"];
const DATE_KEYS: [&str; 2] = ["from_date", "to_date"];
const BOOL_KEYS: [&str; 2] = ["enable_image_understanding", "enable_video_understanding"];
const KNOWN_KEYS: [&str; 9] = [
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

/// Ranking of X results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Latest,
    Top,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Top => "top",
        }
    }
}

/// Validated call parameters with defaults applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Params {
    pub query: String,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub allowed_x_handles: Option<Vec<String>>,
    pub excluded_x_handles: Option<Vec<String>>,
    pub mode: Mode,
    pub max_results: u64,
    pub enable_image_understanding: bool,
    pub enable_video_understanding: bool,
}

/// The tool's JSON schema (`additionalProperties: false`).
pub fn schema() -> Value {
    let date = |what: &str| {
        json!({
            "type": "string",
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
            "description": format!("{what} (YYYY-MM-DD, inclusive)"),
        })
    };
    let handles = |what: &str| {
        json!({
            "type": "array",
            "items": {"type": "string", "pattern": "^[A-Za-z0-9_]{1,15}$"},
            "minItems": 1,
            "maxItems": HANDLES_MAX,
            "uniqueItems": true,
            "description": format!("{what} (X handles without @; cannot be combined with the other handle list)"),
        })
    };
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "maxLength": QUERY_MAX_UNITS,
                "description": "What to look for on X; X search operators are allowed",
            },
            "from_date": date("Earliest post date"),
            "to_date": date("Latest post date"),
            "allowed_x_handles": handles("Only posts from these accounts"),
            "excluded_x_handles": handles("Never posts from these accounts"),
            "mode": {
                "type": "string",
                "enum": ["latest", "top"],
                "default": "latest",
                "description": "latest = newest first, top = most engaging first",
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_RESULTS_LIMIT,
                "default": MAX_RESULTS_DEFAULT,
                "description": "Upper bound on returned posts",
            },
            "enable_image_understanding": {
                "type": "boolean",
                "default": false,
                "description": "Let the search read images attached to posts",
            },
            "enable_video_understanding": {
                "type": "boolean",
                "default": false,
                "description": "Let the search read videos attached to posts",
            },
        },
        "required": ["query"],
        "additionalProperties": false,
    })
}

fn invalid(message: impl Into<String>) -> ToolFailure {
    ToolFailure::new(ErrorCode::InvalidParams, message)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn is_leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// `YYYY-MM-DD` with ASCII digits naming a real calendar date, year 100 or later.
pub fn is_valid_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        bytes[range.clone()]
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| text[range].parse().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    if year < 100 || !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let days = match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= days
}

fn is_valid_handle(handle: &str) -> bool {
    (1..=15).contains(&handle.len())
        && handle
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn handle_list(object: &Map<String, Value>, key: &str) -> Result<Option<Vec<String>>, ToolFailure> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let Some(items) = value.as_array() else {
        return Err(invalid(format!("`{key}` must be an array of X handles")));
    };
    if items.is_empty() {
        return Err(invalid(format!("`{key}` must list at least one handle")));
    }
    let mut handles: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let Some(handle) = item.as_str().filter(|handle| is_valid_handle(handle)) else {
            return Err(invalid(format!(
                "`{key}` entries must be X handles of 1-15 letters, digits or underscores"
            )));
        };
        if handles.iter().any(|seen| seen == handle) {
            return Err(invalid(format!(
                "`{key}` lists \"{handle}\" more than once"
            )));
        }
        handles.push(handle.to_owned());
    }
    Ok(Some(handles))
}

fn bool_key(object: &Map<String, Value>, key: &str) -> Result<bool, ToolFailure> {
    match object.get(key) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid(format!("`{key}` must be true or false"))),
    }
}

fn max_results(object: &Map<String, Value>) -> Result<u64, ToolFailure> {
    let Some(value) = object.get("max_results") else {
        return Ok(MAX_RESULTS_DEFAULT);
    };
    let whole = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.fract() == 0.0 && *number >= 0.0)
            .map(|number| number as u64)
    });
    whole
        .filter(|number| (1..=MAX_RESULTS_LIMIT).contains(number))
        .ok_or_else(|| {
            invalid(format!(
                "`max_results` must be a whole number from 1 to {MAX_RESULTS_LIMIT}"
            ))
        })
}

/// Validates `input` in the spec's order; the first failure wins.
pub fn validate(input: &Value) -> Result<Params, ToolFailure> {
    // 1. Object.
    let Some(object) = input.as_object() else {
        return Err(invalid("parameters must be a JSON object"));
    };
    // 2. Blank query string.
    if let Some(Value::String(query)) = object.get("query")
        && query.trim().is_empty()
    {
        return Err(invalid("`query` must not be blank"));
    }
    // 3. Both handle lists.
    if HANDLE_KEYS.iter().all(|key| object.contains_key(*key)) {
        return Err(ToolFailure::new(
            ErrorCode::InvalidFilters,
            "use either `allowed_x_handles` or `excluded_x_handles`, not both",
        ));
    }
    // 4. Too many handles.
    for key in HANDLE_KEYS {
        if let Some(Value::Array(items)) = object.get(key)
            && items.len() > HANDLES_MAX
        {
            return Err(ToolFailure::new(
                ErrorCode::TooManyHandles,
                format!(
                    "`{key}` lists {} handles; at most {HANDLES_MAX} are allowed",
                    items.len()
                ),
            ));
        }
    }
    // 5. Dates.
    let mut dates: [Option<String>; 2] = [None, None];
    for (slot, key) in dates.iter_mut().zip(DATE_KEYS) {
        if let Some(value) = object.get(key) {
            match value.as_str().filter(|text| is_valid_date(text)) {
                Some(text) => *slot = Some(text.to_owned()),
                None => {
                    return Err(ToolFailure::new(
                        ErrorCode::InvalidDate,
                        format!("`{key}` must be a real calendar date written YYYY-MM-DD"),
                    ));
                }
            }
        }
    }
    let [from_date, to_date] = dates;
    // 6. Range (YYYY-MM-DD sorts chronologically as text).
    if let (Some(from), Some(to)) = (&from_date, &to_date)
        && from > to
    {
        return Err(ToolFailure::new(
            ErrorCode::InvalidDateRange,
            format!("`from_date` {from} is after `to_date` {to}"),
        ));
    }
    // 7. Everything else the schema forbids.
    if let Some(unknown) = object
        .keys()
        .find(|key| !KNOWN_KEYS.contains(&key.as_str()))
    {
        return Err(invalid(format!("unknown parameter `{unknown}`")));
    }
    let query = match object.get("query") {
        None => return Err(invalid("`query` is required")),
        Some(Value::String(query)) => query.clone(),
        Some(_) => return Err(invalid("`query` must be a string")),
    };
    if utf16_len(&query) > QUERY_MAX_UNITS {
        return Err(invalid(format!(
            "`query` is longer than {QUERY_MAX_UNITS} characters"
        )));
    }
    let allowed_x_handles = handle_list(object, HANDLE_KEYS[0])?;
    let excluded_x_handles = handle_list(object, HANDLE_KEYS[1])?;
    let mode = match object.get("mode") {
        None => Mode::Latest,
        Some(value) => match value.as_str() {
            Some("latest") => Mode::Latest,
            Some("top") => Mode::Top,
            _ => return Err(invalid("`mode` must be \"latest\" or \"top\"")),
        },
    };
    let max_results = max_results(object)?;
    let [enable_image_understanding, enable_video_understanding] = [
        bool_key(object, BOOL_KEYS[0])?,
        bool_key(object, BOOL_KEYS[1])?,
    ];
    Ok(Params {
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
