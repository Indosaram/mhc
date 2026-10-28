//! The newest proposal/reply on the active branch, excluding only this tool-call assistant.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentExchange {
    pub proposal: String,
    pub reply: String,
}

pub fn pending_consent_exchange(
    entries: &[Value],
    tool_call_id: Option<&str>,
) -> Vec<ConsentExchange> {
    let mut messages: Vec<_> = entries.iter().filter_map(conversation_message).collect();
    if messages.last().is_some_and(|(role, _, ids)| {
        role == "assistant"
            && tool_call_id.is_some_and(|id| ids.iter().any(|candidate| candidate == id))
    }) {
        messages.pop();
    }
    let [.., (proposal_role, proposal, _), (reply_role, reply, _)] = messages.as_slice() else {
        return Vec::new();
    };
    if proposal_role != "assistant" || reply_role != "user" || proposal.trim().is_empty() {
        return Vec::new();
    }
    vec![ConsentExchange {
        proposal: proposal.clone(),
        reply: reply.clone(),
    }]
}

fn conversation_message(entry: &Value) -> Option<(String, String, Vec<String>)> {
    let message = if entry.get("type").and_then(Value::as_str) == Some("message") {
        entry.get("message")?
    } else {
        entry
    };
    let role = message.get("role")?.as_str()?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let content = message.get("content").or_else(|| message.get("text"));
    let mut ids = Vec::new();
    let text = match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("toolCall")
                    && let Some(id) = block.get("id").and_then(Value::as_str) {
                        ids.push(id.to_owned());
                    }
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    Some((role.to_owned(), text, ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_current_tool_call_assistant_is_excluded() {
        let mut entries = vec![
            json!({"type":"message","message":{"role":"assistant","content":"Delete X?"}}),
            json!({"type":"message","message":{"role":"user","content":"ok"}}),
            json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"current"}]}}),
        ];
        assert_eq!(
            pending_consent_exchange(&entries, Some("current"))[0].proposal,
            "Delete X?"
        );
        assert!(pending_consent_exchange(&entries, Some("earlier")).is_empty());
        entries.pop();
        entries
            .push(json!({"type":"message","message":{"role":"user","content":[{"type":"image"}]}}));
        assert!(pending_consent_exchange(&entries, None).is_empty());
    }
    #[test]
    fn newer_proposals_do_not_reuse_earlier_ok() {
        let entries = vec![
            json!({"role":"assistant","text":"Old?"}),
            json!({"role":"user","text":"ok"}),
            json!({"role":"assistant","text":"New?"}),
        ];
        assert!(pending_consent_exchange(&entries, None).is_empty());
    }
}
