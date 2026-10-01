//! Port of senpi packages/ai/src/api/devin-agent/trailer.ts.
//!
//! Connect end-of-stream trailer errors. Cascade rejects a turn (`invalid_argument`,
//! `permission_denied`, ...) with an HTTP 200 whose only frame is the trailer
//! `{ error: { code, message, details } }`. The trailer is the sole server-side evidence for such a
//! rejection, so it is parsed with guards - never asserted - and its details are folded into the
//! message a user sees.

use serde_json::{Map, Value};

const MAX_TRAILER_EVIDENCE_CHARS: usize = 2000;

/// `DevinTrailerError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevinTrailerError {
    pub code: String,
    pub message: String,
    pub formatted: String,
}

/// `readDevinTrailerError`.
pub fn read_devin_trailer_error(trailer: &str) -> Option<DevinTrailerError> {
    let text = crate::utils::js::trim(trailer);
    if text.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(text).ok()?;
    let error = parsed.as_object()?.get("error")?.as_object()?;
    let code = error.get("code").and_then(Value::as_str).unwrap_or_default();
    let message = error.get("message").and_then(Value::as_str).unwrap_or_default();
    if code.is_empty() && message.is_empty() {
        return None;
    }
    let detail = summarize_details(error.get("details"));
    let suffix = match detail {
        Some(detail) => format!(" [details: {detail}]"),
        None => String::new(),
    };
    let code_prefix = if code.is_empty() { String::new() } else { format!(" {code}") };
    Some(DevinTrailerError {
        code: code.to_owned(),
        message: message.to_owned(),
        formatted: format!("Devin stream error{code_prefix}: {message}{suffix}"),
    })
}

fn summarize_details(details: Option<&Value>) -> Option<String> {
    let details = details?.as_array()?;
    if details.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for entry in details {
        let Some(entry) = entry.as_object() else { continue };
        let entry_type = entry.get("type").and_then(Value::as_str).filter(|value| !value.is_empty());
        let evidence = evidence_of(entry);
        let part = match (entry_type, evidence) {
            (Some(entry_type), Some(evidence)) => format!("{entry_type}: {evidence}"),
            (Some(entry_type), None) => entry_type.to_owned(),
            (None, Some(evidence)) => evidence,
            (None, None) => continue,
        };
        if !part.is_empty() {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return None;
    }
    let summary = parts.join("; ");
    Some(if summary.chars().count() > MAX_TRAILER_EVIDENCE_CHARS {
        format!("{}…", summary.chars().take(MAX_TRAILER_EVIDENCE_CHARS).collect::<String>())
    } else {
        summary
    })
}

fn evidence_of(entry: &Map<String, Value>) -> Option<String> {
    if let Some(debug) = entry.get("debug") {
        if let Some(debug) = debug.as_str() {
            return Some(debug.to_owned());
        }
        return serde_json::to_string(debug).ok();
    }
    entry.get("value").and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned)
}
