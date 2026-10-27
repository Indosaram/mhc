//! Port of senpi's compaction `rejected-recovery.ts`.
use maho_ai::types::Model;
use maho_core::{
    compaction::{compaction::estimate_tokens, settings::CompactionSettings},
    cursor_history_admission::{admit_cursor_history, cursor_admission_budget_bytes},
    messages::filter_context_excluded_messages,
    session_manager::{build_context_entries, build_session_context},
};
use maho_ext_api::CompactionResult;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const REJECTED_RECOVERY_ENTRY: &str = "compaction-recovery-rejected";

/// Shared with core admission: a fallback's conservative estimate cannot overrule a usable summary.
pub fn would_compaction_overflow(path_entries: &[Value], result: &CompactionResult, from_extension: bool, model: &Model, settings: &CompactionSettings) -> bool {
    let Some(current_leaf) = path_entries.last() else { return false };
    let simulated_id = format!("simulated-{}", uuid::Uuid::new_v4());
    let mut entries = path_entries.to_vec();
    entries.push(json!({
        "type": "compaction", "id": simulated_id, "parentId": current_leaf.get("id"),
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "summary": result.summary, "firstKeptEntryId": result.first_kept_entry_id,
        "tokensBefore": result.tokens_before, "details": result.details, "fromHook": from_extension,
    }));
    let mut messages = build_session_context(&entries, Some(&simulated_id)).messages;
    if matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth") {
        let admitted = admit_cursor_history(Some(&messages), cursor_admission_budget_bytes(model.context_window as f64), None);
        messages = admitted.messages.unwrap_or(messages);
    }
    let tokens: u64 = filter_context_excluded_messages(messages).iter().map(estimate_tokens).sum();
    let reserve = crate::policy::resolve_effective_reserve_tokens(model.context_window as f64, settings.reserve_tokens as f64, settings.ideal.reserve_scaling_enabled);
    tokens as f64 > model.context_window as f64 - reserve
}

/// State, not elapsed time or a synthetic message revision, releases the latch.
pub fn compaction_recovery_state_key(branch: &[Value], model: Option<&Model>, settings: &CompactionSettings) -> String {
    let mut hash = Sha256::new();
    let model = model.map(|model| json!([model.provider, model.id, model.api, model.base_url, model.context_window]));
    hash.update(json!({
        "model": model, "reserveTokens": settings.reserve_tokens,
        "reserveScalingEnabled": settings.ideal.reserve_scaling_enabled, "keepRecentTokens": settings.keep_recent_tokens,
    }).to_string());
    for mut entry in build_context_entries(branch, None) {
        // Status/todo/usage bookkeeping is not a new retained context. In particular,
        // the rejection's own durable entry must not release its latch.
        if !matches!(entry.get("type").and_then(Value::as_str), Some("message" | "custom_message" | "compaction" | "context_edit" | "branch_summary" | "configuration_update")) {
            continue;
        }
        // The resident mirror removes summarized messages and reconnects parent IDs.
        // Neither changes the retained context or its identities after reopen.
        if let Some(object) = entry.as_object_mut() {
            object.remove("parentId");
        }
        hash.update(entry.to_string());
        hash.update("\n");
    }
    format!("{:x}", hash.finalize())
}

/// Persisted on the branch, so extension reload and session reopen cannot retry it.
pub fn is_automatic_compaction_blocked(branch: &[Value], model: Option<&Model>, settings: &CompactionSettings) -> bool {
    let rejection = branch.iter().rev().find(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(REJECTED_RECOVERY_ENTRY)
            && entry.get("data").and_then(|data| data.get("failureKind")).and_then(Value::as_str) == Some("unsafe-retained-content")
    });
    rejection
        .and_then(|entry| entry.get("data")?.get("stateKey")?.as_str())
        .is_some_and(|key| key == compaction_recovery_state_key(branch, model, settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::compaction::settings::default_compaction_settings;

    fn model(window: u64) -> Model {
        let mut model: Model = serde_json::from_value(json!({
            "id":"m","name":"m","api":"faux","provider":"faux","baseUrl":"","reasoning":false,"input":["text"],
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":window,"maxTokens":1000
        })).expect("model fixture");
        model.context_window = window;
        model
    }
    fn message(id: &str, parent: Option<&str>, text: &str) -> Value {
        json!({"type":"message","id":id,"parentId":parent,"timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":text,"timestamp":0}})
    }
    fn result(summary: &str) -> CompactionResult {
        CompactionResult { summary: summary.into(), first_kept_entry_id: "b".into(), tokens_before: 1, details: None }
    }
    fn rejection(branch: &[Value], model: &Model, settings: &CompactionSettings) -> Value {
        json!({"type":"custom","id":"r","parentId":"b","timestamp":"2026-01-01T00:00:00.000Z","customType":REJECTED_RECOVERY_ENTRY,
            "data":{"failureKind":"unsafe-retained-content","stateKey":compaction_recovery_state_key(branch, Some(model), settings)}})
    }

    #[test]
    fn overflow_probe_counts_summary_text_against_the_reserve_adjusted_window() {
        let settings = default_compaction_settings();
        let branch = [message("a", None, "old"), message("b", Some("a"), "kept")];
        let model = model(100_000);
        assert!(!would_compaction_overflow(&branch, &result("short"), true, &model, &settings));
        assert!(would_compaction_overflow(&branch, &result(&"word ".repeat(120_000)), true, &model, &settings));
        assert!(!would_compaction_overflow(&[], &result("anything"), true, &model, &settings));
    }

    #[test]
    fn latch_holds_for_unchanged_state_and_releases_on_new_context_or_settings() {
        let settings = default_compaction_settings();
        let model = model(100_000);
        let mut branch = vec![message("a", None, "old"), message("b", Some("a"), "kept")];
        assert!(!is_automatic_compaction_blocked(&branch, Some(&model), &settings));
        branch.push(rejection(&branch, &model, &settings));
        // The rejection's own entry never releases its latch, and reparenting is ignored.
        assert!(is_automatic_compaction_blocked(&branch, Some(&model), &settings));
        branch[1]["parentId"] = json!("elsewhere");
        assert!(is_automatic_compaction_blocked(&branch, Some(&model), &settings));
        let mut other_settings = settings.clone();
        other_settings.keep_recent_tokens += 1;
        assert!(!is_automatic_compaction_blocked(&branch, Some(&model), &other_settings));
        assert!(!is_automatic_compaction_blocked(&branch, Some(&self::model(50_000)), &settings));
        branch.push(message("c", Some("r"), "new request"));
        assert!(!is_automatic_compaction_blocked(&branch, Some(&model), &settings));
    }

    #[test]
    fn only_unsafe_retained_content_rejections_latch() {
        let settings = default_compaction_settings();
        let model = model(100_000);
        let mut branch = vec![message("a", None, "old")];
        let mut entry = rejection(&branch, &model, &settings);
        entry["data"]["failureKind"] = json!("summarization-timeout");
        branch.push(entry);
        assert!(!is_automatic_compaction_blocked(&branch, Some(&model), &settings));
    }
}
