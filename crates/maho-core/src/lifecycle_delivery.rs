//! Transcript-first delivery custody and notice recovery by ticket identity.
use crate::session_manager::SessionManager;
use serde_json::{Value, json};
pub fn message_ticket(message: &Value) -> Option<&str> {
    message["details"]["ticketId"].as_str()
}
pub fn lifecycle_delivery_entries(entries: &[Value]) -> indexmap::IndexMap<String, String> {
    let mut result = indexmap::IndexMap::new();
    for e in entries {
        if e["type"] == "custom_message"
            && let (Some(ticket), Some(id)) = (message_ticket(e), e["id"].as_str()) {
                result.entry(ticket.into()).or_insert_with(|| id.into());
            }
    }
    result
}
pub fn consume_lifecycle_delivery(
    manager: &mut SessionManager,
    active: &[Value],
    run_id: Option<&str>,
    message: &Value,
    consumed: impl FnOnce(&str, Value) -> Result<(), String>,
) -> Result<Option<String>, String> {
    if message["role"] != "custom" {
        return Ok(None);
    }
    let Some(ticket) = message_ticket(message) else {
        return Ok(None);
    };
    let Some(custody) = active.iter().find(|r| r["ticketId"] == ticket) else {
        return Ok(None);
    };
    if custody["state"] == "consumed" {
        return Ok(None);
    }
    let Some(run) = run_id else {
        return Ok(None);
    };
    let existing = lifecycle_delivery_entries(&manager.entries());
    let id = if let Some(id) = existing.get(ticket) {
        id.clone()
    } else {
        let e = manager.append_custom_message(
            message["customType"].as_str().unwrap_or_default(),
            message["content"].clone(),
            message["display"] == true,
            message.get("details").cloned(),
        );
        if let Some(error) = manager.last_persistence_error() {
            return Err(error.to_owned());
        }
        e["id"].as_str().ok_or("missing transcript id")?.into()
    };
    consumed(ticket, json!({"runId":run,"transcriptEntryId":id}))?;
    Ok(Some(id))
}
pub fn held_lifecycle_notice(
    entries: &[Value],
    ticket: &str,
    goal_continuation: bool,
) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| e["type"] == "custom" && e["customType"] == "session-notice")
        .find_map(|e| {
            let d = &e["data"];
            if d["message"]["details"]["ticketId"] == ticket
                || (goal_continuation && d["producer"] == "goal" && d.get("message").is_none())
            {
                d["noticeId"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcript_is_written_once_before_custody_consumption() {
        let mut manager = SessionManager::in_memory("/tmp", None, None);
        let message = json!({"role":"custom","customType":"wake","content":"payload","display":false,"details":{"ticketId":"t"}});
        let active = vec![json!({"ticketId":"t","state":"queued"})];
        let first = consume_lifecycle_delivery(
            &mut manager,
            &active,
            Some("run"),
            &message,
            |ticket, run| {
                assert_eq!(ticket, "t");
                assert!(run["transcriptEntryId"].is_string());
                Ok(())
            },
        )
        .expect("consume");
        let second =
            consume_lifecycle_delivery(&mut manager, &active, Some("run"), &message, |_, _| Ok(()))
                .expect("replay");
        assert_eq!(first, second);
        assert_eq!(
            manager
                .entries()
                .iter()
                .filter(|e| e["type"] == "custom_message")
                .count(),
            1
        );
    }
}
