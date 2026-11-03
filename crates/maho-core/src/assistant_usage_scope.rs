//! Port of senpi packages/coding-agent/src/core/assistant-usage-scope.ts.
//!
//! Branch entries are the session's JSON entries. Upstream builds the projection and asks whether it still
//! carries an assistant message from the entry; the Rust caller answers that through `is_projected`, since
//! the projection is built by the session that owns the branch.

use serde_json::Value;

/// Whether an assistant's provider-reported usage still describes the model context after `context_edit`
/// entries (boundary omissions and replacements) changed that context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssistantUsageScope {
    /// The assistant still contributes a message to the current projection.
    pub projected: bool,
    /// Its usage describes the current context: projected and no later context edit on the branch.
    pub usage_matches_projection: bool,
    /// An explicit overflow error stays recoverable: not omitted by a later edit and not behind a compaction.
    pub retained_for_explicit_recovery: bool,
}

const UNCHANGED: AssistantUsageScope = AssistantUsageScope {
    projected: true,
    usage_matches_projection: true,
    retained_for_explicit_recovery: true,
};

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

/// An assistant without a persisted entry is judged as projected, like the upstream session core.
pub fn resolve_assistant_usage_scope(
    branch: &[Value],
    is_projected: impl FnOnce(&str) -> bool,
    assistant_entry_id: Option<&str>,
) -> AssistantUsageScope {
    let Some(assistant_entry_id) = assistant_entry_id else {
        return UNCHANGED;
    };
    let assistant_index = branch
        .iter()
        .position(|entry| entry.get("id").and_then(Value::as_str) == Some(assistant_entry_id));
    let later: &[Value] = assistant_index.map_or(&[], |index| &branch[index + 1..]);
    let edits_or_compaction = || {
        later
            .iter()
            .any(|entry| matches!(entry_type(entry), Some("context_edit" | "compaction")))
    };
    if assistant_index.is_some() && !edits_or_compaction() {
        return UNCHANGED;
    }
    let projected = is_projected(assistant_entry_id);
    let latest_edit = later.iter().rev().find(|entry| {
        entry_type(entry) == Some("context_edit")
            && entry.get("targetId").and_then(Value::as_str) == Some(assistant_entry_id)
    });
    let omitted =
        latest_edit.is_some_and(|edit| edit.get("replacement").is_some_and(Value::is_null));
    AssistantUsageScope {
        projected,
        usage_matches_projection: projected
            && !later
                .iter()
                .any(|entry| entry_type(entry) == Some("context_edit")),
        retained_for_explicit_recovery: !omitted
            && !later
                .iter()
                .any(|entry| entry_type(entry) == Some("compaction")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_an_assistant_with_no_later_edit_fully_in_scope() {
        let branch = vec![
            json!({"id": "a", "type": "message"}),
            json!({"id": "b", "type": "message"}),
        ];
        assert_eq!(
            resolve_assistant_usage_scope(&branch, |_| false, Some("a")),
            UNCHANGED
        );
        assert_eq!(
            resolve_assistant_usage_scope(&branch, |_| false, None),
            UNCHANGED
        );
    }

    #[test]
    fn an_omitting_edit_drops_recovery_and_any_edit_stales_the_usage() {
        let branch = vec![
            json!({"id": "a", "type": "message"}),
            json!({"id": "e", "type": "context_edit", "targetId": "a", "replacement": null}),
        ];
        let scope = resolve_assistant_usage_scope(&branch, |_| false, Some("a"));
        assert_eq!(
            scope,
            AssistantUsageScope {
                projected: false,
                usage_matches_projection: false,
                retained_for_explicit_recovery: false
            }
        );
        let replaced = vec![
            json!({"id": "a", "type": "message"}),
            json!({"id": "e", "type": "context_edit", "targetId": "a", "replacement": {"role": "assistant"}}),
        ];
        let scope = resolve_assistant_usage_scope(&replaced, |_| true, Some("a"));
        assert_eq!(
            scope,
            AssistantUsageScope {
                projected: true,
                usage_matches_projection: false,
                retained_for_explicit_recovery: true
            }
        );
    }

    #[test]
    fn a_later_compaction_ends_explicit_recovery() {
        let branch = vec![
            json!({"id": "a", "type": "message"}),
            json!({"id": "c", "type": "compaction"}),
        ];
        let scope = resolve_assistant_usage_scope(&branch, |_| true, Some("a"));
        assert_eq!(
            scope,
            AssistantUsageScope {
                projected: true,
                usage_matches_projection: true,
                retained_for_explicit_recovery: false
            }
        );
    }
}
