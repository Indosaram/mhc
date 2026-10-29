//! Thread tool failure taxonomy.
//!
//! Upstream counterpart: `thread/errors.ts`. Every thread tool returns failures as data
//! (`kind: "error"`), never as an exception, and every failure names the next action so the caller
//! can recover in one step.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `THREAD_ERROR_CODES`, in upstream order.
pub const THREAD_ERROR_CODES: &[&str] = &[
    "invalid_arguments",
    "caller_context_missing",
    "not_found",
    "ambiguous_target",
    "scope_denied",
    "name_conflict",
    "not_resumable",
    "orphaned",
    "foreign_live_owner",
    "no_active_turn",
    "turn_conflict",
    "not_steerable",
    "message_too_large",
    "queue_full",
    "cursor_invalid",
    "cursor_stale",
    "idempotency_conflict",
    "idempotency_in_progress",
    "idempotency_uncertain",
    "approval_unavailable",
    "approval_route_locked",
    "partial_commit",
    "unsupported",
    "overloaded",
    "transport_closed",
    "host_unavailable",
    "internal_error",
    "model_not_found",
    "model_ambiguous",
    "thinking_level_unsupported",
    "loop_detected",
    "binding_conflict",
    "binding_mismatch",
    "binding_inactive",
    "stale_revision",
    "stale_token",
    "already_answered",
    "answer_in_progress",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadErrorCode(String);

impl ThreadErrorCode {
    pub fn parse(value: &str) -> Option<Self> {
        THREAD_ERROR_CODES.contains(&value).then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ThreadErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn is_thread_error_code(value: &str) -> bool {
    THREAD_ERROR_CODES.contains(&value)
}

/// The data-shaped failure every thread tool returns on the error branch of its result union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadToolFailure {
    pub code: ThreadErrorCode,
    pub message: String,
    pub next_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Builds a failure for a known code; an unknown code panics exactly as upstream throws.
pub fn thread_tool_failure(code: &str, message: impl Into<String>, next_action: impl Into<String>, details: Option<Value>) -> ThreadToolFailure {
    let parsed = ThreadErrorCode::parse(code).unwrap_or_else(|| panic!("unknown thread error code: {code}"));
    ThreadToolFailure { code: parsed, message: message.into(), next_action: next_action.into(), details }
}

/// A details object built from one key, as most call sites need.
pub fn details_of(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Option<Value> {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_and_unknown_codes_are_refused() {
        for code in THREAD_ERROR_CODES {
            assert_eq!(ThreadErrorCode::parse(code).map(|parsed| parsed.to_string()), Some((*code).to_owned()));
        }
        assert!(ThreadErrorCode::parse("nope").is_none());
    }

    #[test]
    fn failure_carries_details_only_when_given() {
        let plain = thread_tool_failure("not_found", "gone", "call thread_list", None);
        assert!(plain.details.is_none());
        let detailed = thread_tool_failure("ambiguous_target", "two", "pick one", details_of([("candidates", Value::Array(vec![]))]));
        assert_eq!(detailed.details.unwrap()["candidates"], Value::Array(vec![]));
    }
}
