//! Quality gate verdicts.

use serde_json::Value;

use crate::quality_gate_fields::{invalid, literal_str, text_field};

pub fn passed_verdict(value: Option<&Value>, field: &str) -> String {
    let s = value.and_then(|v| v.as_str()).unwrap_or("");
    if s == "not_applicable" {
        invalid(format!("{field} must not be not_applicable."), field);
        return "passed".to_string();
    }
    literal_str(value, "passed", field)
}

pub fn code_quality_status_field(value: Option<&Value>, field: &str) -> String {
    let s = value.and_then(|v| v.as_str()).unwrap_or("");
    if s == "CLEAR" || s == "WATCH" {
        return s.to_string();
    }
    invalid(format!("{field} must be CLEAR or WATCH."), field);
    "CLEAR".to_string()
}

pub fn adversarial_verdict(
    row: &serde_json::Map<String, Value>,
    field: &str,
) -> (String, Option<String>) {
    let value = row.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
    if value == "passed" {
        return ("passed".to_string(), None);
    }
    if value == "not_applicable" {
        let reason_field = format!("{field}.reason");
        let reason = text_field(row.get("reason"), &reason_field);
        return ("not_applicable".to_string(), Some(reason));
    }
    invalid(
        format!("{field} must be passed or not_applicable with a reason."),
        field,
    );
    ("passed".to_string(), None)
}
