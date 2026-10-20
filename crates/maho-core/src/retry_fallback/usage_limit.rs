use maho_ai::types::{AssistantErrorCause, AssistantMessage, UsageLimitScope};
use serde_json::Value;

pub fn usage_limit_scope(message: &AssistantMessage) -> Option<UsageLimitScope> {
    match message.error_cause.as_ref() {
        Some(AssistantErrorCause::UsageLimit { scope }) => Some(*scope),
        None => None,
    }
}

pub fn describe_usage_limit_message(message: &AssistantMessage) -> AssistantMessage {
    let mut projected = message.clone();
    match usage_limit_scope(message) {
        Some(UsageLimitScope::Account) => {
            if let Some(raw) = message.error_message.as_deref().filter(|raw| !raw.is_empty()) {
                let raw = maho_ai::utils::provider_failure_description::strip_turn_retry_suppression_prefix(raw);
                projected.error_message = Some(if raw.contains("To keep going now, ") {
                    raw.rsplit_once(" (retry-after-ms: ").filter(|(_, suffix)| {
                        suffix.strip_suffix(')').is_some_and(|millis| !millis.is_empty() && millis.bytes().all(|byte| byte.is_ascii_digit()))
                    }).map_or_else(|| raw.clone(), |(message, _)| message.to_owned())
                } else {
                    let reset = raw.split_whitespace().collect::<Vec<_>>();
                    let reset = reset.iter().position(|word| word.eq_ignore_ascii_case("reset") || word.eq_ignore_ascii_case("resets"))
                        .map(|index| reset[index.saturating_add(1)..].join(" "))
                        .map(|reset| reset.split(['"', '}', '\n']).next().unwrap_or_default().trim_end_matches(['.', ';']).trim().to_owned())
                        .filter(|reset| !reset.is_empty())
                        .map(|reset| format!(" (resets {reset})")).unwrap_or_default();
                    format!("The {} account usage limit has been reached{reset}. Switch to another provider with /model, or configure another provider in retry.fallbackChains.", message.provider)
                });
            }
        }
        Some(UsageLimitScope::Model) | None => {}
    }
    projected
}

pub fn describe_usage_limit_entry(mut entry: Value) -> Value {
    if entry["type"] == "message" && entry["message"]["role"] == "assistant"
        && let Ok(message) = serde_json::from_value::<AssistantMessage>(entry["message"].clone())
    {
        let projected = describe_usage_limit_message(&message);
        if projected.error_message != message.error_message {
            entry["message"]["errorMessage"] = projected.error_message.map_or(Value::Null, Value::String);
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_projection_preserves_cause_and_identity_without_classifying_text() {
        // Given
        let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        message.error_message = Some("diagnostic".into());
        message.error_cause = Some(AssistantErrorCause::UsageLimit { scope: UsageLimitScope::Account });
        let original = serde_json::json!({"type":"message", "id":"entry", "parentId":"parent", "message":message});
        // When
        let projected = describe_usage_limit_entry(original.clone());
        // Then
        assert_eq!(projected["id"], original["id"]);
        assert_eq!(projected["parentId"], original["parentId"]);
        assert_eq!(projected["message"]["errorCause"], original["message"]["errorCause"]);
        assert_ne!(projected["message"]["errorMessage"], original["message"]["errorMessage"]);
        message.error_cause = None;
        message.error_message = Some("usage_limit_reached".into());
        assert!(usage_limit_scope(&message).is_none());
        assert_eq!(describe_usage_limit_message(&message), message);
        message.error_cause = Some(AssistantErrorCause::UsageLimit { scope: UsageLimitScope::Model });
        assert_eq!(describe_usage_limit_message(&message), message);
    }
}
