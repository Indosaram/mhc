//! Client correlation classification (senpi `client-message-events.ts` `correlatedRecord`): the record kinds that
//! carry the active client correlation, so one without it answers no client message.
use serde_json::Value;

pub fn correlated_record(record: &Value) -> bool {
    let Some(kind) = record.get("type").and_then(Value::as_str) else {
        return false;
    };
    if kind == "entry_appended" {
        return record["entry"]["type"] == "message";
    }
    matches!(
        kind,
        "turn_start" | "turn_end" | "session_abort" | "agent_end"
    ) || kind.starts_with("message_")
        || kind.starts_with("tool_execution_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn turn_message_tool_and_message_entries_are_correlated() {
        for kind in [
            "turn_start",
            "turn_end",
            "message_update",
            "tool_execution_end",
            "session_abort",
            "agent_end",
        ] {
            assert!(correlated_record(&json!({"type":kind})), "{kind}");
        }
        assert!(correlated_record(
            &json!({"type":"entry_appended","entry":{"type":"message"}})
        ));
        assert!(!correlated_record(
            &json!({"type":"entry_appended","entry":{"type":"label"}})
        ));
        assert!(!correlated_record(&json!({"type":"agent_start"})));
        assert!(!correlated_record(&json!({"kind":"turn_start"})));
    }
}
