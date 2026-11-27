//! Designated model refusal tracking and chain exhaustion (`manager/model-refusals.ts`).

use std::sync::{Arc, Mutex, PoisonError};

use regex::Regex;
use std::sync::LazyLock;

use crate::manager::child_handle::ManagedChildEvent;
use crate::manager::credential_failure::{
    is_credential_failure, runtime_fallback_candidates, terminal_failure_message, usage_limit_scope,
};
use crate::runners::{RunnerFailure, RunnerFailureKind};
use crate::state::{ModelRefusal, ModelRefusalReason, TaskRecord, TaskRunStats};
use crate::store::TaskRecordStore;

pub const MODEL_CHAIN_EXHAUSTED: &str = "model_chain_exhausted";

const DETAIL_MAX: usize = 200;

static RESETS_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?iu)\bresets?\s+((?:at|in)\s+)?([^.\n;]+)").expect("resets regex")
});
static POLICY_REFUSAL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\bflagged\b|content[_ ]?(?:policy|filter|management)|usage[_ ]policy|safety (?:system|policy)|\brefus(?:ed|al)\b|\bviolat(?:es|ed|ion)\b",
    )
    .expect("policy refusal regex")
});
static UNAVAILABLE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)overload|\b(?:500|502|503|504|529)\b|unavailable|capacity|timed? ?out|rate[_ ]limit|too many requests",
    )
    .expect("unavailable regex")
});

fn reason_label(reason: ModelRefusalReason) -> &'static str {
    match reason {
        ModelRefusalReason::UsageLimit => "usage limit",
        ModelRefusalReason::NotSignedIn => "not signed in",
        ModelRefusalReason::Refused => "refused",
        ModelRefusalReason::Unavailable => "overloaded or unavailable",
        ModelRefusalReason::Error => "failed",
    }
}

/// The model itself failed the turn: not a kill, not a host or session failure.
#[must_use]
pub fn is_model_turn_failure(killed: bool, failure: &RunnerFailure) -> bool {
    !killed
        && matches!(
            failure.kind,
            RunnerFailureKind::ChildTurnFailed | RunnerFailureKind::ChildPromptFailed
        )
}

/// A model turn failure the manager may walk past: no tool call ran, so the next designated model
/// can take the same prompt.
#[must_use]
pub fn is_chain_walkable(
    killed: bool,
    failure: &RunnerFailure,
    run_stats: Option<&TaskRunStats>,
) -> bool {
    is_model_turn_failure(killed, failure) && run_stats.map_or(0, |stats| stats.tool_calls) == 0
}

#[must_use]
pub fn reason_of_failure(message: &str, model_id: Option<&str>) -> ModelRefusalReason {
    if usage_limit_scope(message).is_some() {
        return ModelRefusalReason::UsageLimit;
    }
    if is_credential_failure(message, model_id) {
        return ModelRefusalReason::NotSignedIn;
    }
    if POLICY_REFUSAL_REGEX.is_match(message) {
        return ModelRefusalReason::Refused;
    }
    if UNAVAILABLE_REGEX.is_match(message) {
        return ModelRefusalReason::Unavailable;
    }
    ModelRefusalReason::Error
}

fn reason_of_engine_fallback(
    event: &ManagedChildEvent,
    detail: Option<&str>,
) -> ModelRefusalReason {
    if event.limit.is_some() {
        return ModelRefusalReason::UsageLimit;
    }
    if let Some(detail) = detail {
        let classified = reason_of_failure(detail, None);
        if classified != ModelRefusalReason::Error {
            return classified;
        }
    }
    match event.reason.as_deref() {
        Some("billing") => ModelRefusalReason::UsageLimit,
        Some("refusal") => ModelRefusalReason::Refused,
        Some("transient") => ModelRefusalReason::Unavailable,
        _ => ModelRefusalReason::Error,
    }
}

#[must_use]
pub fn build_model_refusal(
    model: &str,
    reason: ModelRefusalReason,
    message: Option<&str>,
    at: &str,
) -> ModelRefusal {
    let detail = message.and_then(first_line);
    let resets = message.and_then(resets_of);
    ModelRefusal {
        model: base_selector(model).to_owned(),
        reason,
        at: at.to_owned(),
        detail,
        resets,
    }
}

#[must_use]
pub fn refusal_of(record: &TaskRecord, message: &str, at: &str) -> ModelRefusal {
    build_model_refusal(
        &record.model,
        reason_of_failure(
            message,
            record
                .resolved_model
                .as_ref()
                .map(|model| model.model_id.as_str()),
        ),
        Some(message),
        at,
    )
}

#[must_use]
pub fn with_refusal(mut record: TaskRecord, refusal: ModelRefusal) -> TaskRecord {
    record
        .model_refusals
        .get_or_insert_with(Vec::new)
        .push(refusal);
    record
}

/// Records each designated model the child's own in-session fallback walks past.
pub fn observe_model_refusals(
    store: TaskRecordStore,
    task_id: String,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
) -> impl Fn(&ManagedChildEvent) + Send + Sync {
    let failed: Mutex<Option<String>> = Mutex::new(None);
    move |event| {
        if event.event_type == "message_end"
            && let Some(message) = &event.message
            && message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
        {
            *failed.lock().unwrap_or_else(PoisonError::into_inner) = failed_turn_message(message);
            return;
        }
        if event.event_type != "retry_fallback_applied" {
            return;
        }
        let Ok(Some(record)) = store.load(&task_id) else {
            return;
        };
        let detail = failed.lock().unwrap_or_else(PoisonError::into_inner).take();
        let model = event.from.as_deref().unwrap_or(&record.model);
        let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now())
            .map(|ts| ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
        let refusal = build_model_refusal(
            model,
            reason_of_engine_fallback(event, detail.as_deref()),
            detail.as_deref(),
            &at,
        );
        let _ = store.mutate(&task_id, |fresh: &TaskRecord| {
            with_refusal(fresh.clone(), refusal.clone())
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFailureFields {
    pub error_message: String,
    pub failure_kind: Option<String>,
}

/// Terminal error of a failed run. When the designated chain is spent on recognized refusals,
/// lists every designated model tried and why (`model_chain_exhausted`).
#[must_use]
pub fn terminal_failure_fields(
    record: &TaskRecord,
    failure: &RunnerFailure,
    model_failure: bool,
) -> TerminalFailureFields {
    let message = terminal_failure_message(Some(record), &failure.message);
    let final_refusal = refusal_of(record, &failure.message, &record.updated_at);
    if !model_failure
        || final_refusal.reason == ModelRefusalReason::Error
        || !runtime_fallback_candidates(record, &failure.message)
            .remaining
            .is_empty()
    {
        return TerminalFailureFields {
            error_message: message,
            failure_kind: (failure.kind == RunnerFailureKind::ChildUnresponsive)
                .then(|| RunnerFailureKind::ChildUnresponsive.as_str().to_owned()),
        };
    }
    let mut all = record.model_refusals.clone().unwrap_or_default();
    all.push(final_refusal);
    TerminalFailureFields {
        error_message: format!("{message}\n{}", describe_exhausted_chain(record, &all)),
        failure_kind: Some(MODEL_CHAIN_EXHAUSTED.to_owned()),
    }
}

#[must_use]
pub fn describe_exhausted_chain(record: &TaskRecord, refusals: &[ModelRefusal]) -> String {
    let subject = match (&record.category, &record.agent_type) {
        (Some(category), _) => format!("category \"{category}\""),
        (None, Some(agent)) => format!("agent \"{agent}\""),
        (None, None) => "this task".to_owned(),
    };
    let mut lines = vec![format!(
        "Every designated model for {subject} was refused, and a child never runs on any other model:"
    )];
    for refusal in refusals {
        lines.push(format!(
            "- {}: {}{}",
            refusal.model,
            reason_label(refusal.reason),
            refusal_suffix(refusal)
        ));
    }
    lines.join("\n")
}

fn refusal_suffix(refusal: &ModelRefusal) -> String {
    if let Some(resets) = &refusal.resets {
        return format!(" (resets {resets})");
    }
    if refusal.reason == ModelRefusalReason::UsageLimit {
        return String::new();
    }
    match &refusal.detail {
        Some(detail) => format!(" ({detail})"),
        None => String::new(),
    }
}

fn failed_turn_message(message: &serde_json::Value) -> Option<String> {
    let stop_reason = message.get("stopReason")?.as_str()?;
    if stop_reason != "error" && stop_reason != "aborted" {
        return None;
    }
    let text = message
        .get("errorMessage")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    Some(text.unwrap_or_else(|| format!("stopped with {stop_reason}")))
}

fn first_line(message: &str) -> Option<String> {
    let line = message
        .split('\n')
        .map(str::trim)
        .find(|part| !part.is_empty())?;
    let mut chars = line.chars();
    let truncated: String = chars.by_ref().take(DETAIL_MAX - 1).collect();
    if chars.next().is_some() {
        Some(format!("{truncated}…"))
    } else {
        Some(line.to_owned())
    }
}

fn resets_of(message: &str) -> Option<String> {
    let captures = RESETS_REGEX.captures(message)?;
    let prefix = captures.get(1).map_or("", |m| m.as_str());
    let when = without_unbalanced_paren(captures.get(2).map(|m| m.as_str().trim()))?;
    if when.is_empty() {
        None
    } else {
        Some(format!("{prefix}{when}"))
    }
}

fn without_unbalanced_paren(value: Option<&str>) -> Option<String> {
    let value = value?;
    let opened = value.matches('(').count();
    let closed = value.matches(')').count();
    if closed > opened && value.ends_with(')') {
        Some(value[..value.len() - 1].trim().to_owned())
    } else {
        Some(value.to_owned())
    }
}

#[must_use]
pub fn base_selector(selector: &str) -> &str {
    let slash = selector.find('/');
    let colon = selector.rfind(':');
    match (slash, colon) {
        (Some(s), Some(c)) if c > s => &selector[..c],
        _ => selector,
    }
}
