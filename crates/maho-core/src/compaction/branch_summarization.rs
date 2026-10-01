//! Port of senpi packages/coding-agent/src/core/compaction/branch-summarization.ts (entry
//! collection and preparation; generateBranchSummary itself needs the provider streaming stack
//! and is recorded as partial in the parity ledger).

use std::collections::HashSet;

use maho_ai::types::Usage;
use serde_json::Value;

use super::compaction::{CompactionPreparation, estimate_tokens};
use super::settings::CompactionSettings;
use super::utils::{FileOperations, create_file_ops, extract_file_ops_from_message};
use crate::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    is_context_excluded_custom_message,
};
use crate::session_manager::SessionManager;

/// senpi BranchSummaryResult.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BranchSummaryResult {
    pub summary: Option<String>,
    pub usage: Option<Usage>,
    pub read_files: Option<Vec<String>>,
    pub modified_files: Option<Vec<String>>,
    pub aborted: Option<bool>,
    pub error: Option<String>,
}

/// senpi BranchSummaryDetails.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// senpi BranchPreparation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchPreparation {
    pub messages: Vec<Value>,
    pub file_ops: FileOperations,
    pub total_tokens: i64,
}

/// senpi CollectEntriesResult.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectEntriesResult {
    pub entries: Vec<Value>,
    pub common_ancestor_id: Option<String>,
}

fn entry_id(entry: &Value) -> Option<String> {
    entry.get("id").and_then(Value::as_str).map(str::to_string)
}

/// senpi collectEntriesForBranchSummary.
pub fn collect_entries_for_branch_summary(
    session: &SessionManager,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> CollectEntriesResult {
    let Some(old_leaf_id) = old_leaf_id else {
        return CollectEntriesResult::default();
    };

    let old_path: HashSet<String> = session.branch(Some(old_leaf_id)).iter().filter_map(entry_id).collect();
    let target_path = session.branch(Some(target_id));

    let mut common_ancestor_id: Option<String> = None;
    for entry in target_path.iter().rev() {
        if let Some(id) = entry_id(entry)
            && old_path.contains(&id)
        {
            common_ancestor_id = Some(id);
            break;
        }
    }

    let mut entries: Vec<Value> = Vec::new();
    let mut current = Some(old_leaf_id.to_string());
    while let Some(id) = current {
        if Some(id.clone()) == common_ancestor_id {
            break;
        }
        let Some(entry) = session.entry(&id) else {
            break;
        };
        current = entry.get("parentId").and_then(Value::as_str).map(str::to_string);
        entries.push(entry);
    }

    entries.reverse();
    CollectEntriesResult { entries, common_ancestor_id }
}

/// senpi getMessageFromEntry.
pub fn get_message_from_entry(entry: &Value) -> Option<Value> {
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => {
            let message = entry.get("message")?;
            if message.get("role").and_then(Value::as_str) == Some("toolResult") {
                return None;
            }
            Some(message.clone())
        }
        Some("custom_message") => {
            let custom_type = entry.get("customType").and_then(Value::as_str).unwrap_or_default();
            if is_context_excluded_custom_message(custom_type) {
                return None;
            }
            Some(create_custom_message(
                custom_type,
                entry.get("content").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
                entry.get("display").and_then(Value::as_bool).unwrap_or(false),
                entry.get("details").cloned(),
                entry.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
            ))
        }
        Some("branch_summary") => Some(create_branch_summary_message(
            entry.get("summary").and_then(Value::as_str).unwrap_or_default(),
            entry.get("fromId").and_then(Value::as_str).unwrap_or_default(),
            entry.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
        )),
        Some("compaction") => Some(create_compaction_summary_message(
            entry.get("summary").and_then(Value::as_str).unwrap_or_default(),
            entry.get("tokensBefore").and_then(Value::as_i64).unwrap_or(0),
            entry.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
            entry.get("details").cloned(),
        )),
        _ => None,
    }
}

/// senpi prepareBranchEntries: newest-first selection under a token budget.
pub fn prepare_branch_entries(entries: &[Value], token_budget: i64) -> BranchPreparation {
    let mut messages: Vec<Value> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens: i64 = 0;

    for entry in entries {
        if entry.get("type").and_then(Value::as_str) == Some("branch_summary")
            && !entry.get("fromHook").and_then(Value::as_bool).unwrap_or(false)
            && let Some(details) = entry.get("details")
        {
            if let Some(read_files) = details.get("readFiles").and_then(Value::as_array) {
                for path in read_files.iter().filter_map(Value::as_str) {
                    file_ops.read.insert(path.to_string());
                }
            }
            if let Some(modified_files) = details.get("modifiedFiles").and_then(Value::as_array) {
                for path in modified_files.iter().filter_map(Value::as_str) {
                    file_ops.edited.insert(path.to_string());
                }
            }
        }
    }

    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry) else {
            continue;
        };

        extract_file_ops_from_message(&message, &mut file_ops);
        let tokens = estimate_tokens(&message) as i64;

        if token_budget > 0 && total_tokens + tokens > token_budget {
            let entry_type = entry.get("type").and_then(Value::as_str);
            if matches!(entry_type, Some("compaction") | Some("branch_summary"))
                && (total_tokens as f64) < token_budget as f64 * 0.9
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation { messages, file_ops, total_tokens }
}

/// senpi BRANCH_SUMMARY_PREAMBLE.
pub const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.
Summary of that exploration:

";

/// senpi BRANCH_SUMMARY_PROMPT.
pub const BRANCH_SUMMARY_PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// senpi createBranchCompactionPreparation.
pub fn create_branch_compaction_preparation(
    entries: &[Value],
    preparation: &BranchPreparation,
    reserve_tokens: i64,
) -> CompactionPreparation {
    let first_entry_id = entries.first().and_then(entry_id).unwrap_or_default();
    let mut settings: CompactionSettings = super::settings::default_compaction_settings();
    settings.enabled = true;
    settings.reserve_tokens = reserve_tokens;
    settings.keep_recent_tokens = 0;

    CompactionPreparation {
        first_kept_entry_id: first_entry_id,
        messages_to_summarize: preparation.messages.clone(),
        source_messages: Vec::new(),
        turn_prefix_messages: Vec::new(),
        turn_prefix_source_messages: Vec::new(),
        is_split_turn: false,
        tokens_before: preparation.total_tokens,
        previous_summary: None,
        file_ops: preparation.file_ops.clone(),
        settings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message_entry(id: &str, parent: Option<&str>, message: Value) -> Value {
        json!({ "id": id, "parentId": parent, "type": "message", "message": message })
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text, "timestamp": 0 })
    }

    fn assistant(text: &str) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "text", "text": text }], "timestamp": 0 })
    }

    fn tool_result(id: &str) -> Value {
        json!({ "role": "toolResult", "toolCallId": id, "content": [{ "type": "text", "text": "body" }], "timestamp": 0 })
    }

    #[test]
    fn tool_results_are_skipped_but_custom_and_summary_entries_convert() {
        assert!(get_message_from_entry(&message_entry("a", None, tool_result("call"))).is_none());
        assert!(get_message_from_entry(&message_entry("b", None, user("hi"))).is_some());

        let custom = json!({ "id": "c", "type": "custom_message", "customType": "note", "content": "body", "display": true, "timestamp": "2026-01-01T00:00:00.000Z" });
        let converted = get_message_from_entry(&custom).expect("custom message");
        assert_eq!(converted.get("role").and_then(Value::as_str), Some("custom"));

        let branch = json!({ "id": "d", "type": "branch_summary", "summary": "earlier", "fromId": "x", "timestamp": "2026-01-01T00:00:00.000Z" });
        let converted = get_message_from_entry(&branch).expect("branch summary");
        assert_eq!(converted.get("role").and_then(Value::as_str), Some("branchSummary"));

        let compaction = json!({ "id": "e", "type": "compaction", "summary": "digest", "tokensBefore": 12, "timestamp": "2026-01-01T00:00:00.000Z" });
        let converted = get_message_from_entry(&compaction).expect("compaction summary");
        assert_eq!(converted.get("role").and_then(Value::as_str), Some("compactionSummary"));

        let label = json!({ "id": "f", "type": "label", "targetId": "a", "label": "x" });
        assert!(get_message_from_entry(&label).is_none());
    }

    #[test]
    fn preparation_walks_newest_first_and_keeps_chronological_order() {
        let entries = vec![
            message_entry("a", None, user("one")),
            message_entry("b", Some("a"), assistant("two")),
        ];
        let preparation = prepare_branch_entries(&entries, 0);
        assert_eq!(preparation.messages.len(), 2);
        assert_eq!(preparation.messages[0].get("content").and_then(Value::as_str), Some("one"));
        assert!(preparation.total_tokens > 0);
    }

    #[test]
    fn the_token_budget_drops_the_oldest_messages() {
        let entries = vec![
            message_entry("a", None, user(&"x".repeat(400))),
            message_entry("b", Some("a"), user("small")),
        ];
        let preparation = prepare_branch_entries(&entries, 10);
        assert_eq!(preparation.messages.len(), 1);
        assert_eq!(preparation.messages[0].get("content").and_then(Value::as_str), Some("small"));
    }

    #[test]
    fn file_operations_come_from_tool_calls_and_previous_summaries() {
        let entries = vec![
            json!({ "id": "s", "type": "branch_summary", "summary": "earlier", "fromId": "x", "timestamp": "2026-01-01T00:00:00.000Z",
                    "details": { "readFiles": ["/read"], "modifiedFiles": ["/modified"] } }),
            message_entry("a", None, json!({ "role": "assistant", "content": [{ "type": "toolCall", "name": "read", "arguments": { "path": "/live" } }], "timestamp": 0 })),
        ];
        let preparation = prepare_branch_entries(&entries, 0);
        assert!(preparation.file_ops.read.contains("/read"));
        assert!(preparation.file_ops.read.contains("/live"));
        assert!(preparation.file_ops.edited.contains("/modified"));
    }

    #[test]
    fn a_compaction_summary_fits_even_when_it_exceeds_the_budget() {
        let entries = vec![
            json!({ "id": "c", "type": "compaction", "summary": &"y".repeat(400), "tokensBefore": 1, "timestamp": "2026-01-01T00:00:00.000Z" }),
        ];
        let preparation = prepare_branch_entries(&entries, 10);
        assert_eq!(preparation.messages.len(), 1);
        assert_eq!(preparation.messages[0].get("role").and_then(Value::as_str), Some("compactionSummary"));
    }

    #[test]
    fn the_preparation_carries_the_first_entry_id_and_a_zero_keep_budget() {
        let entries = vec![message_entry("first", None, user("hi"))];
        let preparation = prepare_branch_entries(&entries, 0);
        let compaction_preparation = create_branch_compaction_preparation(&entries, &preparation, 16_384);
        assert_eq!(compaction_preparation.first_kept_entry_id, "first");
        assert!(compaction_preparation.settings.enabled);
        assert_eq!(compaction_preparation.settings.reserve_tokens, 16_384);
        assert_eq!(compaction_preparation.settings.keep_recent_tokens, 0);
        assert!(!compaction_preparation.is_split_turn);
        assert_eq!(compaction_preparation.tokens_before, preparation.total_tokens);
    }

    #[test]
    fn collecting_without_a_previous_position_yields_nothing() {
        let session = SessionManager::in_memory("/tmp", None, Some(Vec::new()));
        let collected = collect_entries_for_branch_summary(&session, None, "target");
        assert!(collected.entries.is_empty());
        assert_eq!(collected.common_ancestor_id, None);
    }
}
