//! Port of senpi `terminal/tools/monitor-schema.ts`: the monitor input helpers. The flat JSON
//! schema itself is served by `tools::monitor::monitor_schema` (owned by the monitor tool); this
//! module carries the branch predicates and value resolvers the schema file exports.
use serde_json::Value;

pub use super::monitor::{DEFAULT_MONITOR_TIMEOUT_MS, MAX_MONITOR_TIMEOUT_MS};

fn non_empty_str<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// `isFileCreateInput`: a description and a path.
pub fn is_file_create_input(input: &Value) -> bool {
    non_empty_str(input, "description").is_some() && non_empty_str(input, "path").is_some()
}

/// `isCreateInput`: a description and a command.
pub fn is_create_input(input: &Value) -> bool {
    non_empty_str(input, "description").is_some() && non_empty_str(input, "command").is_some()
}

/// `resolveDimension`: a finite value of at least 1, truncated; otherwise the fallback.
pub fn resolve_dimension(value: Option<f64>, fallback: u16) -> u16 {
    match value {
        Some(value) if value.is_finite() && value >= 1.0 => {
            value.trunc().min(f64::from(u16::MAX)) as u16
        }
        _ => fallback,
    }
}

/// `resolveTimeoutMs`: default 300000, truncated and clamped to 1..=3600000.
pub fn resolve_timeout_ms(value: Option<f64>) -> u64 {
    let timeout = value.unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS as f64);
    timeout.trunc().clamp(1.0, MAX_MONITOR_TIMEOUT_MS as f64) as u64
}

/// `compileFilter`: `None` without a filter; the JavaScript RegExp dialect maps to fancy-regex.
pub fn compile_filter(
    filter: Option<&str>,
) -> Result<Option<fancy_regex::Regex>, fancy_regex::Error> {
    filter.map(fancy_regex::Regex::new).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn branch_predicates() {
        assert!(is_file_create_input(&json!({"description":"d","path":"p"})));
        assert!(!is_file_create_input(&json!({"description":"","path":"p"})));
        assert!(is_create_input(&json!({"description":"d","command":"c"})));
        assert!(!is_create_input(&json!({"description":"d","command":""})));
    }

    #[test]
    fn resolvers() {
        assert_eq!(resolve_dimension(None, 80), 80);
        assert_eq!(resolve_dimension(Some(0.5), 80), 80);
        assert_eq!(resolve_dimension(Some(f64::NAN), 80), 80);
        assert_eq!(resolve_dimension(Some(120.9), 80), 120);
        assert_eq!(resolve_timeout_ms(None), 300_000);
        assert_eq!(resolve_timeout_ms(Some(0.0)), 1);
        assert_eq!(resolve_timeout_ms(Some(9e9)), 3_600_000);
        assert_eq!(resolve_timeout_ms(Some(1500.7)), 1500);
        assert!(compile_filter(None).unwrap().is_none());
        assert!(
            compile_filter(Some("err(or)?"))
                .unwrap()
                .unwrap()
                .is_match("error")
                .unwrap()
        );
        assert!(compile_filter(Some("(")).is_err());
    }
}
