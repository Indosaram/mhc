pub mod rewrite;
pub mod scan;
pub mod search;

use std::time::Instant;

use serde_json::Map;
use serde_json::Value;

pub const LANGUAGES: [&str; 25] = crate::pattern_hints::LANGUAGES;
pub const STRICTNESS: [&str; 5] = ["cst", "smart", "ast", "relaxed", "signature"];
pub const MAX_PATHS: usize = 64;
pub const MAX_GLOBS: usize = 32;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const PROJECT_CWD_ENV: &str = "OMO_AST_GREP_PROJECT_CWD";

pub fn code_points(value: &str) -> usize {
    value.chars().count()
}

pub fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub fn as_object(input: &Value) -> Result<&Map<String, Value>, String> {
    input
        .as_object()
        .ok_or_else(|| "Input must be an object".to_owned())
}

pub fn reject_unknown(obj: &Map<String, Value>, known: &[&str]) -> Result<(), String> {
    match obj.keys().find(|key| !known.contains(&key.as_str())) {
        Some(key) => Err(format!("Unknown property: {key}")),
        None => Ok(()),
    }
}

pub fn integer_in(value: &Value, min: u64, max: u64) -> Option<u64> {
    let number = value.as_f64()?;
    let in_range = number.fract() == 0.0 && number >= min as f64 && number <= max as f64;
    in_range.then_some(number as u64)
}

pub fn optional_bool(obj: &Map<String, Value>, flag: &str) -> Result<Option<bool>, String> {
    match obj.get(flag) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{flag} must be a boolean")),
    }
}

pub fn max_matches(obj: &Map<String, Value>) -> Result<u64, String> {
    use crate::sg_runner::DEFAULT_MATCHES;
    use crate::sg_runner::MAX_MATCHES;
    match obj.get("maxMatches") {
        None => Ok(DEFAULT_MATCHES),
        Some(value) => integer_in(value, 1, MAX_MATCHES)
            .ok_or_else(|| format!("maxMatches must be an integer between 1 and {MAX_MATCHES}")),
    }
}

pub fn timeout_ms(obj: &Map<String, Value>) -> Result<u64, String> {
    use crate::sg_runner::DEFAULT_TIMEOUT_MS;
    use crate::sg_runner::MAX_TIMEOUT_MS;
    match obj.get("timeoutMs") {
        None => Ok(DEFAULT_TIMEOUT_MS),
        Some(value) => integer_in(value, MIN_TIMEOUT_MS, MAX_TIMEOUT_MS).ok_or_else(|| {
            format!("timeoutMs must be an integer between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}")
        }),
    }
}

pub fn string_list(
    obj: &Map<String, Value>,
    key: &str,
    singular: &str,
    min: usize,
    max: usize,
    max_chars: usize,
) -> Result<Option<Vec<String>>, String> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array"))?;
    if items.len() < min || items.len() > max {
        return Err(if min == 0 {
            format!("{key} must have at most {max} entries")
        } else {
            format!("{key} must have {min}-{max} entries")
        });
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let text = item
            .as_str()
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("each {singular} must be a non-empty string"))?;
        if code_points(text) > max_chars {
            return Err(format!(
                "each {singular} must be at most {max_chars} characters"
            ));
        }
        out.push(text.to_owned());
    }
    Ok(Some(out))
}

pub fn default_workdir() -> String {
    std::env::var(PROJECT_CWD_ENV).unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_owned())
    })
}

pub fn scope_args(
    globs: Option<&[String]>,
    include_hidden: bool,
    follow_symlinks: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    for glob in globs.unwrap_or_default() {
        args.push("--globs".to_owned());
        args.push(glob.clone());
    }
    if include_hidden {
        args.extend(["--no-ignore".to_owned(), "hidden".to_owned()]);
    }
    if follow_symlinks {
        args.push("--follow".to_owned());
    }
    args
}

pub fn distinct_paths(matches: &[Map<String, Value>]) -> usize {
    let mut paths: Vec<&str> = matches
        .iter()
        .filter_map(|m| m.get("path").and_then(Value::as_str))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths.len()
}
