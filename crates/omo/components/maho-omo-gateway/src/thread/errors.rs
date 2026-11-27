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
    "reply_target_mismatch",
    "ambiguous_target",
    "scope_denied",
    "name_conflict",
    "not_resumable",
    "orphaned",
    "foreign_live_owner",
    "no_active_turn",
    "turn_conflict",
    "turn_active",
    "not_steerable",
    "message_too_large",
    "queue_full",
    "cursor_invalid",
    "cursor_stale",
    "idempotency_conflict",
    "idempotency_in_progress",
    "idempotency_uncertain",
    "delivered_unconfirmed",
    "prompt_not_started",
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
    "pair_loop_guard",
    "chain_count_limit",
    "binding_conflict",
    "binding_mismatch",
    "binding_inactive",
    "stale_revision",
    "stale_token",
    "already_answered",
    "answer_in_progress",
    "device_offline",
    "relay_requires_paid_plan",
    "alias_taken",
    "gateway_schema_too_new",
    "remote_unavailable",
    "account_unavailable",
    "device_not_found",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadErrorCode(String);

impl ThreadErrorCode {
    pub fn parse(value: &str) -> Option<Self> {
        THREAD_ERROR_CODES
            .contains(&value)
            .then(|| Self(value.to_owned()))
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
pub fn thread_tool_failure(
    code: &str,
    message: impl Into<String>,
    next_action: impl Into<String>,
    details: Option<Value>,
) -> ThreadToolFailure {
    let parsed =
        ThreadErrorCode::parse(code).unwrap_or_else(|| panic!("unknown thread error code: {code}"));
    ThreadToolFailure {
        code: parsed,
        message: message.into(),
        next_action: next_action.into(),
        details,
    }
}

/// A details object built from one key, as most call sites need.
pub fn details_of(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Option<Value> {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

const UPDATE_COMMAND: &str = "mhc update";

/// Turns a gateway `gateway_schema_too_new` error into a user-facing refusal naming both schema
/// versions and the remedy. Returns `None` for extension schemas or any other error.
pub fn gateway_schema_refusal(
    code: &str,
    found: Option<i64>,
    supported: Option<i64>,
    extension: Option<&str>,
) -> Option<ThreadToolFailure> {
    if code != "gateway_schema_too_new" || extension.is_some() {
        return None;
    }
    let (found, supported) = (found?, supported?);
    Some(thread_tool_failure(
        "gateway_schema_too_new",
        format!(
            "This mhc build reads gateway schema {supported}, but this machine's gateway store is at schema {found}: a newer build already upgraded it. Nothing was sent or changed."
        ),
        format!("Update mhc with `{UPDATE_COMMAND}`, restart this session, then retry."),
        details_of([
            ("gateway_schema", Value::from(found)),
            ("binary_schema", Value::from(supported)),
            ("update_command", Value::from(UPDATE_COMMAND)),
        ]),
    ))
}

/// Parses a serialized/error-string schema-too-new error into a user-facing refusal.
pub fn gateway_schema_refusal_message(msg: &str) -> Option<ThreadToolFailure> {
    if !msg.contains("gateway_schema_too_new") {
        return None;
    }
    let ver = super::schema::GATEWAY_MIGRATIONS.len() as i64;
    gateway_schema_refusal("gateway_schema_too_new", Some(ver + 1), Some(ver), None)
}

/// Pair loop guard message (`takePairToken`).
pub fn pair_loop_guard_message(sender: &str, target: &str, burst: i64, window_ms: i64) -> String {
    let secs = window_ms / 1000;
    format!("{burst} sends from {sender} to {target} within {secs} s; this one was not delivered.")
}

pub fn pair_loop_guard_next_action(retry_after_ms: i64) -> String {
    let secs = (retry_after_ms + 999) / 1000;
    format!(
        "This looks like a send loop between these two sessions; the next send to this target is accepted after {secs} s."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_and_unknown_codes_are_refused() {
        for code in THREAD_ERROR_CODES {
            assert_eq!(
                ThreadErrorCode::parse(code).map(|parsed| parsed.to_string()),
                Some((*code).to_owned())
            );
        }
        assert!(ThreadErrorCode::parse("nope").is_none());
    }

    #[test]
    fn failure_carries_details_only_when_given() {
        let plain = thread_tool_failure("not_found", "gone", "call thread_list", None);
        assert!(plain.details.is_none());
        let detailed = thread_tool_failure(
            "ambiguous_target",
            "two",
            "pick one",
            details_of([("candidates", Value::Array(vec![]))]),
        );
        assert_eq!(
            detailed.details.unwrap()["candidates"],
            Value::Array(vec![])
        );
    }

    #[test]
    fn gateway_schema_refusal_parses_too_new_and_ignores_other_errors() {
        let refusal = gateway_schema_refusal("gateway_schema_too_new", Some(99), Some(16), None)
            .expect("schema too new refusal");
        assert_eq!(refusal.code.as_str(), "gateway_schema_too_new");
        let parsed = gateway_schema_refusal_message("Error: gateway_schema_too_new");
        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap().code.as_str(), "gateway_schema_too_new");
        assert!(gateway_schema_refusal_message("SQLITE_BUSY: database is locked").is_none());
    }

    #[test]
    fn pair_loop_guard_formats_window_and_retry_seconds() {
        let msg = pair_loop_guard_message("alice", "bob", 6, 30_000);
        assert!(msg.contains("alice"));
        assert!(msg.contains("bob"));
        assert!(msg.contains("30 s"));
        let next = pair_loop_guard_next_action(2_100);
        assert!(next.contains("3 s"));
    }
}
