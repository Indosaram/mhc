//! senpi `ttsr/follow-up-limit.ts`: one corrective follow-up per rule since the user's last message.
use crate::types::TTSR_INJECTION_CUSTOM_TYPE;
use serde_json::Value;

pub const TTSR_LOOP_STOPPED_ENTRY_TYPE: &str = "ttsr-loop-stopped";
pub const TTSR_LOOP_STOPPED_EVENT: &str = "ttsr:loop-stopped";

pub fn ttsr_loop_stopped_notice(rule: &str) -> String {
    format!(
        "The {rule} stream rule flagged the reply again after its correction, so no further correction was sent. Send any message to continue."
    )
}

fn nudged_rules(entry: &Value) -> &[Value] {
    if entry.get("type").and_then(Value::as_str) != Some("custom_message")
        || entry.get("customType").and_then(Value::as_str) != Some(TTSR_INJECTION_CUSTOM_TYPE)
    {
        return &[];
    }
    entry
        .pointer("/details/rules")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Whether this rule already sent its one corrective follow-up since the user's last message. Read
/// from the session, so a host that rebuilds the extension between turns cannot re-arm it
/// (senpi#2967); the engine-wide turn bound covers every other source.
pub fn rule_already_corrected(entries: &[Value], rule: &str) -> bool {
    for entry in entries.iter().rev() {
        if entry.get("type").and_then(Value::as_str) == Some("message")
            && entry.pointer("/message/role").and_then(Value::as_str) == Some("user")
        {
            return false;
        }
        if nudged_rules(entry)
            .iter()
            .any(|nudged| nudged.as_str() == Some(rule))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nudge(rules: Value) -> Value {
        json!({"type": "custom_message", "customType": TTSR_INJECTION_CUSTOM_TYPE, "details": {"rules": rules}})
    }
    fn user() -> Value {
        json!({"type": "message", "message": {"role": "user", "content": "go"}})
    }

    #[test]
    fn a_nudge_since_the_last_user_message_marks_the_rule_corrected() {
        let entries = [
            user(),
            nudge(json!(["repetitive-turns"])),
            json!({"type": "message", "message": {"role": "assistant"}}),
        ];
        assert!(rule_already_corrected(&entries, "repetitive-turns"));
        assert!(!rule_already_corrected(&entries, "other-rule"));
    }

    #[test]
    fn a_user_message_after_the_nudge_rearms_the_rule() {
        let entries = [nudge(json!(["repetitive-turns"])), user()];
        assert!(!rule_already_corrected(&entries, "repetitive-turns"));
        assert!(!rule_already_corrected(&[], "repetitive-turns"));
    }

    #[test]
    fn malformed_or_foreign_entries_do_not_count() {
        let entries = [
            user(),
            json!({"type": "custom", "customType": TTSR_INJECTION_CUSTOM_TYPE, "data": {"rules": ["r"]}}),
            json!({"type": "custom_message", "customType": TTSR_INJECTION_CUSTOM_TYPE, "details": {"rules": "r"}}),
            json!({"type": "custom_message", "customType": "other", "details": {"rules": ["r"]}}),
        ];
        assert!(!rule_already_corrected(&entries, "r"));
    }

    #[test]
    fn notice_names_the_rule() {
        assert_eq!(
            ttsr_loop_stopped_notice("repetitive-turns"),
            "The repetitive-turns stream rule flagged the reply again after its correction, so no further correction was sent. Send any message to continue."
        );
    }
}
