use crate::lsp::errors::LspError;
use crate::lsp::types::SeverityFilter;
use serde_json::Map;
use serde_json::Value;

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub(crate) fn require_string(params: &Map<String, Value>, key: &str) -> Result<String, LspError> {
    match params.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(LspError::other(format!(
            "Missing required string parameter '{key}'"
        ))),
    }
}

pub(crate) fn optional_string(params: &Map<String, Value>, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(crate) fn require_number(params: &Map<String, Value>, key: &str) -> Result<f64, LspError> {
    optional_number(params, key)
        .ok_or_else(|| LspError::other(format!("Missing required number parameter '{key}'")))
}

pub(crate) fn optional_number(params: &Map<String, Value>, key: &str) -> Option<f64> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

pub(crate) fn optional_boolean(params: &Map<String, Value>, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

pub(crate) fn severity_filter(params: &Map<String, Value>) -> SeverityFilter {
    match params.get("severity").and_then(Value::as_str) {
        Some("error") => SeverityFilter::Error,
        Some("warning") => SeverityFilter::Warning,
        Some("information") => SeverityFilter::Information,
        Some("hint") => SeverityFilter::Hint,
        _ => SeverityFilter::All,
    }
}

/// LSP positions are unsigned; JS forwarded the raw number, so truncate toward zero.
pub(crate) fn position_arg(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Renders a JS number the way template literals do (`3`, not `3.0`).
pub(crate) fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

pub(crate) fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn given_required_string_parameter_when_value_is_missing_or_empty_then_throws_clear_error() {
        let valid = Map::from_iter([("filePath".to_string(), json!("index.ts"))]);
        assert_eq!(require_string(&valid, "filePath").unwrap(), "index.ts");

        let empty_map = Map::new();
        assert_eq!(
            require_string(&empty_map, "filePath").unwrap_err().to_string(),
            "Missing required string parameter 'filePath'"
        );

        let empty_str = Map::from_iter([("filePath".to_string(), json!(""))]);
        assert_eq!(
            require_string(&empty_str, "filePath").unwrap_err().to_string(),
            "Missing required string parameter 'filePath'"
        );

        let number_val = Map::from_iter([("filePath".to_string(), json!(42))]);
        assert_eq!(
            require_string(&number_val, "filePath").unwrap_err().to_string(),
            "Missing required string parameter 'filePath'"
        );
    }

    #[test]
    fn given_required_number_parameter_when_value_is_non_finite_then_throws_clear_error() {
        let valid = Map::from_iter([("line".to_string(), json!(3))]);
        assert_eq!(require_number(&valid, "line").unwrap(), 3.0);

        let empty_map = Map::new();
        assert_eq!(
            require_number(&empty_map, "line").unwrap_err().to_string(),
            "Missing required number parameter 'line'"
        );

        // Note: JSON syntax cannot represent NaN or Infinity; serde_json parses null or fails.
        let null_val = Map::from_iter([("line".to_string(), Value::Null)]);
        assert_eq!(
            require_number(&null_val, "line").unwrap_err().to_string(),
            "Missing required number parameter 'line'"
        );

        let str_val = Map::from_iter([("line".to_string(), json!("3"))]);
        assert_eq!(
            require_number(&str_val, "line").unwrap_err().to_string(),
            "Missing required number parameter 'line'"
        );
    }

    #[test]
    fn given_optional_parameters_when_values_have_wrong_type_then_ignores_them() {
        let str_valid = Map::from_iter([("query".to_string(), json!("symbol"))]);
        assert_eq!(optional_string(&str_valid, "query"), Some("symbol".to_string()));

        let str_wrong = Map::from_iter([("query".to_string(), json!(7))]);
        assert_eq!(optional_string(&str_wrong, "query"), None);

        let num_valid = Map::from_iter([("limit".to_string(), json!(10))]);
        assert_eq!(optional_number(&num_valid, "limit"), Some(10.0));

        let num_wrong = Map::from_iter([("limit".to_string(), json!("10"))]);
        assert_eq!(optional_number(&num_wrong, "limit"), None);

        let bool_valid = Map::from_iter([("includeDeclaration".to_string(), json!(false))]);
        assert_eq!(optional_boolean(&bool_valid, "includeDeclaration"), Some(false));

        let bool_wrong = Map::from_iter([("includeDeclaration".to_string(), json!("false"))]);
        assert_eq!(optional_boolean(&bool_wrong, "includeDeclaration"), None);
    }

    #[test]
    fn given_severity_parameter_when_value_is_unknown_then_defaults_to_all() {
        let warning = Map::from_iter([("severity".to_string(), json!("warning"))]);
        assert_eq!(severity_filter(&warning), SeverityFilter::Warning);

        let unknown = Map::from_iter([("severity".to_string(), json!("fatal"))]);
        assert_eq!(severity_filter(&unknown), SeverityFilter::All);

        let empty = Map::new();
        assert_eq!(severity_filter(&empty), SeverityFilter::All);
    }
}
