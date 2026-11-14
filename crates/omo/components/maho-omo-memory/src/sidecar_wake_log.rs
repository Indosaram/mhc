//! The reporting half of `Rte.onWake` (`rte.ts`): one `wakes.ndjson` line per settled wake plus the
//! `omo-kibitzer:gate` / `omo-kibitzer:unavailable` session notices.
//!
//! Ported from the same bundle as `sidecar_owner.rs`. The append seam here is synchronous, so
//! upstream's promise chain collapses into the single call in `WakeReporter::report`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use memory_core::support::time::format_rfc3339_millis;
use memory_core::sync::redact::contains_secret_like_material;

use crate::kibitzer_events_text::{utf16_head, utf16_len};
use crate::kibitzer_sidecar_outcome::{
    KibitzerWakeCause, KibitzerWakeConfiguration, KibitzerWakeConfigurationCause,
    KibitzerWakeOutcome, KibitzerWakeStatus, KibitzerWakeUsage,
};
use crate::sidecar_sweep::sidecar_dir;

pub const GATE_ENTRY_TYPE: &str = "omo-kibitzer:gate";
pub const UNAVAILABLE_ENTRY_TYPE: &str = "omo-kibitzer:unavailable";
pub const WAKES_FILE_NAME: &str = "wakes.ndjson";
pub const GATE_AFTER_CONSECUTIVE_FAILURES: u32 = 3;
const NOTICE_TEXT_MAX_CHARS: usize = 128;
const MISSING_PROVIDER_MAX_CHARS: usize = 64;
const WAKE_RECORD_MAX_CHARS: usize = 4096;
const NUDGED_PATH_MAX_CHARS: usize = 256;

pub fn wakes_file(recall: &Path, session_id: &str) -> PathBuf {
    sidecar_dir(recall, session_id).join(WAKES_FILE_NAME)
}

pub struct WakeLogDeps<'a> {
    pub recall: &'a Path,
    pub append: &'a dyn Fn(&Path, &str, &str),
    pub warn: &'a dyn Fn(&str),
    pub now_ms: &'a dyn Fn() -> i64,
}

pub type AppendEntry = std::sync::Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

#[derive(Default)]
pub struct WakeReporter {
    streaks: std::sync::Mutex<BTreeMap<String, GateStreak>>,
    unavailable_sent: std::sync::Mutex<BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug)]
struct GateStreak {
    count: u32,
    notified: bool,
}

impl WakeReporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(
        &self,
        outcome: &KibitzerWakeOutcome,
        deps: &WakeLogDeps<'_>,
        append_entry: &AppendEntry,
        category: Option<&str>,
    ) {
        let record = wake_record(outcome, (deps.now_ms)());
        (deps.append)(deps.recall, &outcome.session_id, &record);
        self.report_notice(outcome, append_entry, category);
    }

    fn report_notice(
        &self,
        outcome: &KibitzerWakeOutcome,
        append_entry: &AppendEntry,
        category: Option<&str>,
    ) {
        let Some(configuration) = &outcome.configuration else {
            if !outcome.diagnostic {
                self.streaks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&outcome.session_id);
                return;
            }
            let mut streaks = self
                .streaks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let streak = streaks
                .entry(outcome.session_id.clone())
                .or_insert(GateStreak {
                    count: 0,
                    notified: false,
                });
            streak.count += 1;
            if streak.count < GATE_AFTER_CONSECUTIVE_FAILURES || streak.notified {
                return;
            }
            streak.notified = true;
            let entry = gate_entry(outcome, streak.count, category);
            drop(streaks);
            append_entry(GATE_ENTRY_TYPE, entry);
            return;
        };
        if !self
            .unavailable_sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(outcome.session_id.clone())
        {
            return;
        }
        append_entry(UNAVAILABLE_ENTRY_TYPE, unavailable_entry(configuration));
    }

    pub fn forget(&self, session_id: &str) {
        self.streaks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
        self.unavailable_sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }
}

fn gate_entry(
    outcome: &KibitzerWakeOutcome,
    consecutive_failures: u32,
    category: Option<&str>,
) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "version": 1,
        "status": "failed",
        "cause": cause_str(outcome.cause),
        "candidateCount": outcome.candidate_count,
        "consecutiveFailures": consecutive_failures,
        "wake": outcome.wake,
    });
    let object = entry.as_object_mut().expect("object literal");
    if let Some(model) = &outcome.model {
        object.insert("model".into(), cap(model, NOTICE_TEXT_MAX_CHARS).into());
    }
    if let Some(category) = category {
        object.insert(
            "category".into(),
            cap(category, NOTICE_TEXT_MAX_CHARS).into(),
        );
    }
    if let Some(reason) = &outcome.reason {
        object.insert("reason".into(), reason.clone().into());
    }
    entry
}

fn unavailable_entry(configuration: &KibitzerWakeConfiguration) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "version": 1,
        "category": cap(&configuration.category, NOTICE_TEXT_MAX_CHARS),
        "cause": configuration_cause_str(configuration.cause),
    });
    if let Some(missing) = &configuration.missing_providers {
        entry
            .as_object_mut()
            .expect("object literal")
            .insert("missingProviders".into(), capped_providers(missing).into());
    }
    entry
}

pub fn wake_record(outcome: &KibitzerWakeOutcome, now_ms: i64) -> String {
    let mut record = serde_json::json!({
        "version": 1,
        "at": format_rfc3339_millis(now_ms),
        "sessionId": outcome.session_id,
        "wake": outcome.wake,
        "generation": outcome.generation,
        "status": status_str(outcome.status),
        "candidateCount": outcome.candidate_count,
        "nudged": outcome
            .nudges
            .iter()
            .map(|nudge| cap(&nudge.path, NUDGED_PATH_MAX_CHARS))
            .collect::<Vec<_>>(),
        "steered": outcome.steered,
        "toolCalls": outcome.tool_calls,
        "durationMs": outcome.duration_ms,
        "slotWaitMs": outcome.slot_wait_ms,
        "diagnostic": outcome.diagnostic,
    });
    let object = record.as_object_mut().expect("object literal");
    if let Some(cause) = outcome.cause {
        object.insert("cause".into(), cause_str(Some(cause)).into());
    }
    if let Some(reason) = &outcome.reason {
        object.insert("reason".into(), reason.clone().into());
    }
    if let Some(configuration) = &outcome.configuration {
        object.insert("configuration".into(), configuration_value(configuration));
    }
    if let Some(model) = &outcome.model {
        object.insert("model".into(), cap(model, NOTICE_TEXT_MAX_CHARS).into());
    }
    if let Some(cursors) = &outcome.cursors {
        object.insert(
            "cursors".into(),
            serde_json::json!({ "first": cursors.first, "last": cursors.last }),
        );
    }
    if let Some(context_tokens) = outcome.context_tokens {
        object.insert("contextTokens".into(), context_tokens.into());
    }
    if let Some(usage) = &outcome.usage {
        object.insert("usage".into(), usage_value(usage));
    }
    let line = serde_json::to_string(&record).expect("a JSON value always serializes");
    if line.len() < WAKE_RECORD_MAX_CHARS {
        return format!("{line}\n");
    }
    let object = record.as_object_mut().expect("object literal");
    object.remove("reason");
    object.insert("nudged".into(), serde_json::json!([]));
    object.insert("truncated".into(), true.into());
    format!(
        "{line}\n",
        line = serde_json::to_string(&record).expect("a JSON value always serializes")
    )
}

fn capped_providers(missing: &[String]) -> Vec<String> {
    missing
        .iter()
        .take(16)
        .map(|provider| cap(provider, MISSING_PROVIDER_MAX_CHARS))
        .collect()
}

fn configuration_value(configuration: &KibitzerWakeConfiguration) -> serde_json::Value {
    let mut value = serde_json::json!({
        "category": cap(&configuration.category, NOTICE_TEXT_MAX_CHARS),
        "cause": configuration_cause_str(configuration.cause),
    });
    if let Some(missing) = &configuration.missing_providers {
        value
            .as_object_mut()
            .expect("object literal")
            .insert("missingProviders".into(), capped_providers(missing).into());
    }
    value
}

fn usage_value(usage: &KibitzerWakeUsage) -> serde_json::Value {
    serde_json::json!({
        "input": usage.input,
        "output": usage.output,
        "cacheRead": usage.cache_read,
        "cacheWrite": usage.cache_write,
    })
}

fn cap(text: &str, max_chars: usize) -> String {
    if contains_secret_like_material(text) {
        return "redacted".to_string();
    }
    if utf16_len(text) <= max_chars {
        text.to_string()
    } else {
        utf16_head(text, max_chars)
    }
}

fn status_str(status: KibitzerWakeStatus) -> &'static str {
    match status {
        KibitzerWakeStatus::Completed => "completed",
        KibitzerWakeStatus::ToolBudgetExceeded => "tool_budget_exceeded",
        KibitzerWakeStatus::Deadline => "deadline",
        KibitzerWakeStatus::Failed => "failed",
        KibitzerWakeStatus::Cancelled => "cancelled",
    }
}

fn cause_str(cause: Option<KibitzerWakeCause>) -> &'static str {
    match cause {
        Some(KibitzerWakeCause::ChildFailed) => "child_failed",
        Some(KibitzerWakeCause::ChildFailedUpstream) => "child_failed_upstream",
        Some(KibitzerWakeCause::StartFailed) => "start_failed",
        Some(KibitzerWakeCause::Shutdown) => "shutdown",
        None => "failed",
    }
}

fn configuration_cause_str(cause: KibitzerWakeConfigurationCause) -> &'static str {
    match cause {
        KibitzerWakeConfigurationCause::CategoryUnavailable => "category_unavailable",
        KibitzerWakeConfigurationCause::BeyondCategory => "beyond_category",
    }
}

#[cfg(test)]
#[path = "sidecar_wake_log_tests.rs"]
mod tests;
