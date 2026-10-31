//! Port of senpi packages/coding-agent/src/core/environment-context.ts.
//!
//! Environment context (working directory + current date) delivered to the model as an append-only,
//! hidden user-role custom message instead of as system prompt text (senpi#2093), so the system prompt
//! stays byte-stable for provider prefix caches. On the wire the message is the leading content block
//! of the user message it precedes (senpi#2118).
//!
//! Messages are the JSON wire shape used across maho-core. Upstream marks converted environment
//! messages by object identity; here the caller passes their indices.

use std::collections::HashSet;

use serde_json::{Value, json};

pub const ENVIRONMENT_CONTEXT_MESSAGE_TYPE: &str = "environment-context";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentContext {
    /// Working directory with `/` separators.
    pub cwd: String,
    /// UTC calendar date, `YYYY-MM-DD`.
    pub current_date: String,
}

pub fn resolve_environment_context(
    cwd: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> EnvironmentContext {
    EnvironmentContext {
        cwd: cwd.replace('\\', "/"),
        current_date: now.format("%Y-%m-%d").to_string(),
    }
}

pub fn format_environment_context(context: &EnvironmentContext) -> String {
    [
        "<environment_context>".to_owned(),
        format!("  <cwd>{}</cwd>", context.cwd),
        format!("  <current_date>{}</current_date>", context.current_date),
        "</environment_context>".to_owned(),
    ]
    .join("\n")
}

fn environment_context_of(details: &Value) -> Option<EnvironmentContext> {
    Some(EnvironmentContext {
        cwd: details.get("cwd")?.as_str()?.to_owned(),
        current_date: details.get("currentDate")?.as_str()?.to_owned(),
    })
}

pub fn latest_environment_context(messages: &[Value]) -> Option<EnvironmentContext> {
    messages.iter().rev().find_map(|message| {
        if message.get("role").and_then(Value::as_str) != Some("custom")
            || message.get("customType").and_then(Value::as_str)
                != Some(ENVIRONMENT_CONTEXT_MESSAGE_TYPE)
        {
            return None;
        }
        environment_context_of(message.get("details")?)
    })
}

pub fn create_environment_context_message(context: &EnvironmentContext, timestamp: i64) -> Value {
    json!({
        "role": "custom",
        "customType": ENVIRONMENT_CONTEXT_MESSAGE_TYPE,
        "content": format_environment_context(context),
        "display": false,
        "details": { "cwd": context.cwd, "currentDate": context.current_date },
        "timestamp": timestamp,
    })
}

/// A new environment context message when `current` differs from the latest one in `messages` (or none
/// is visible, e.g. a fresh session or a compaction that summarized it away); `None` when the model
/// already sees it.
pub fn environment_context_message_if_changed(
    messages: &[Value],
    current: &EnvironmentContext,
    timestamp: i64,
) -> Option<Value> {
    if latest_environment_context(messages).as_ref() == Some(current) {
        return None;
    }
    Some(create_environment_context_message(current, timestamp))
}

fn content_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn is_user(message: Option<&Value>) -> bool {
    message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        == Some("user")
}

/// Merge each converted environment-context message (by index in `messages`) into the user message that
/// immediately follows it, as that message's leading content block(s). One that no user message follows
/// stays a standalone user message.
pub fn fold_environment_context_into_next_user_message(
    messages: &[Value],
    environment_messages: &HashSet<usize>,
) -> Vec<Value> {
    let mut folded = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        let next = messages.get(index + 1);
        if environment_messages.contains(&index)
            && is_user(Some(message))
            && is_user(next)
            && !environment_messages.contains(&(index + 1))
            && let Some(next) = next
        {
            let mut merged = next.clone();
            let mut blocks = content_blocks(message.get("content"));
            blocks.extend(content_blocks(next.get("content")));
            if let Some(object) = merged.as_object_mut() {
                object.insert("content".to_owned(), Value::Array(blocks));
            }
            folded.push(merged);
            index += 2;
            continue;
        }
        folded.push(message.clone());
        index += 1;
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(cwd: &str, date: &str) -> EnvironmentContext {
        EnvironmentContext {
            cwd: cwd.to_owned(),
            current_date: date.to_owned(),
        }
    }

    #[test]
    fn resolves_and_formats_the_context() {
        let Some(now) = chrono::DateTime::from_timestamp(1_791_996_420, 0) else {
            panic!("timestamp")
        };
        let resolved = resolve_environment_context("C:\\work\\repo", now);
        assert_eq!(resolved, context("C:/work/repo", "2026-10-14"));
        assert_eq!(
            format_environment_context(&resolved),
            "<environment_context>\n  <cwd>C:/work/repo</cwd>\n  <current_date>2026-10-14</current_date>\n</environment_context>"
        );
    }

    #[test]
    fn appends_only_when_the_visible_context_changes() {
        let first = context("/w", "2026-10-30");
        let messages = vec![create_environment_context_message(&first, 1)];
        assert_eq!(latest_environment_context(&messages), Some(first.clone()));
        assert!(environment_context_message_if_changed(&messages, &first, 2).is_none());
        let moved = context("/w2", "2026-10-30");
        let message = environment_context_message_if_changed(&messages, &moved, 3);
        assert_eq!(
            message.and_then(|m| m.get("details").cloned()),
            Some(json!({"cwd": "/w2", "currentDate": "2026-10-30"}))
        );
        assert!(environment_context_message_if_changed(&[], &first, 4).is_some());
    }

    #[test]
    fn folds_into_the_next_user_message_and_leaves_a_trailing_one_standalone() {
        let messages = vec![
            json!({"role": "user", "content": "<env/>"}),
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}], "timestamp": 5}),
            json!({"role": "assistant", "content": []}),
            json!({"role": "user", "content": "<env2/>"}),
        ];
        let folded =
            fold_environment_context_into_next_user_message(&messages, &HashSet::from([0, 3]));
        assert_eq!(folded.len(), 3);
        assert_eq!(
            folded[0],
            json!({"role": "user", "content": [{"type": "text", "text": "<env/>"}, {"type": "text", "text": "hi"}], "timestamp": 5})
        );
        assert_eq!(folded[2], messages[3]);
    }
}
