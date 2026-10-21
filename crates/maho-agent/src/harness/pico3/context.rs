use std::collections::BTreeMap;
use super::types::*;

pub async fn derive_context(storage: &dyn Storage, conversation_id: Id, at: Option<Id>, ctx: &Context) -> Result<ContextView> {
    let before_at = at.map(|id| id.saturating_add(1));
    let head = storage.scan_entries(EntryScan { conversation_id, with_head: Some(true), before: before_at, kind: None, limit: 1 }, ctx).await?.into_iter().next();
    let from = head.as_ref().and_then(|entry| entry.head);
    let mut range = Vec::new();
    let mut before = before_at;
    loop {
        let page = storage.scan_entries(EntryScan { conversation_id, before, limit: 256, with_head: None, kind: None }, ctx).await?;
        let mut done = page.len() < 256;
        let last = page.last().map(|entry| entry.id);
        for entry in page {
            if from.is_some_and(|from| entry.id < from) { done = true; break; }
            range.push(entry);
        }
        if done { break; }
        before = last;
    }
    range.reverse();
    let mut edits = BTreeMap::new();
    for entry in &range {
        for edit in entry.edits.iter().flatten() {
            let target = match edit { ContextEdit::Omit { target } | ContextEdit::Replace { target, .. } => *target };
            edits.insert(target, edit.clone());
        }
    }
    let entries: Vec<_> = match &head {
        None => range,
        Some(head) => std::iter::once(head.clone()).chain(range.into_iter().filter(|entry| entry.head.is_none())).collect(),
    };
    let mut messages = Vec::new();
    for entry in &entries {
        match edits.get(&entry.id) {
            Some(ContextEdit::Omit { .. }) => {},
            Some(ContextEdit::Replace { messages: replacement, .. }) => messages.extend(replacement.iter().flatten().cloned()),
            None => messages.extend(entry.model.iter().flatten().cloned()),
        }
    }
    Ok(ContextView { head, entries, messages: reorder_tool_results(messages) })
}

fn role(message: &StoredMessage) -> Option<&str> { message.as_json().get("role").and_then(JsonValue::as_str) }

fn reorder_tool_results(messages: Vec<StoredMessage>) -> Vec<StoredMessage> {
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(message) = messages.get(index) {
        out.push(message.clone());
        index = index.saturating_add(1);
        if role(message) != Some("assistant") { continue; }
        let calls: Vec<_> = message.as_json().get("content").and_then(JsonValue::as_array).into_iter().flatten().filter(|block| block.get("type").and_then(JsonValue::as_str) == Some("toolCall")).collect();
        if calls.is_empty() { continue; }
        let mut results = BTreeMap::new();
        while let Some(result) = messages.get(index).filter(|message| role(message) == Some("toolResult")) {
            if let Some(JsonValue::String(id)) = result.as_json().get("toolCallId") { results.insert(id.clone(), result.clone()); }
            index = index.saturating_add(1);
        }
        for call in calls {
            let id = call.get("id").cloned().unwrap_or(JsonValue::Null);
            if let JsonValue::String(id) = &id
                && let Some(result) = results.get(id) { out.push(result.clone()); continue; }
            let text = JsonValue::object(vec![("type".into(), JsonValue::String("text".into())), ("text".into(), JsonValue::String("Tool result unavailable: history ends before this call completed.".into()))]);
            out.push(StoredMessage::from_json(JsonValue::object(vec![
                ("role".into(), JsonValue::String("toolResult".into())),
                ("toolCallId".into(), id),
                ("toolName".into(), call.get("name").cloned().unwrap_or(JsonValue::Null)),
                ("content".into(), JsonValue::array(vec![text])),
                ("isError".into(), JsonValue::Bool(true)),
                ("details".into(), JsonValue::object(vec![("reason".into(), JsonValue::String("missing_after_fork".into()))])),
                ("timestamp".into(), message.as_json().get("timestamp").cloned().unwrap_or(JsonValue::Null)),
            ])));
        }
    }
    out
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
