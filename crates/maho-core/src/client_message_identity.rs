//! Port of senpi packages/coding-agent/src/core/client-message-identity.ts.

use maho_ai::types::ImageContent;
use maho_ext_api::InputSource;
use serde_json::Value;

const MAX_ID_LEN: usize = 256;

/// Correlation metadata only, never the rest of a prompt's options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientMessageIdentity {
    pub client_message_id: Option<String>,
    pub client_turn_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    Steer,
    FollowUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedClientInput {
    pub identity: ClientMessageIdentity,
    pub text: String,
    pub images: Option<Vec<ImageContent>>,
    pub mode: QueueMode,
    pub enqueue_order: u64,
    /// The input's `input` event source, kept so restored input is re-stamped with it. `None`: unknown origin.
    pub source: Option<InputSource>,
}

fn identity_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        None => None,
        Some(Value::String(text)) if !text.is_empty() && text.chars().count() <= MAX_ID_LEN => {
            Some(text.clone())
        }
        Some(_) => None,
    }
}

/// `readClientMessageIdentity`: a field that fails the schema (non-string, empty, over 256 chars) fails
/// the whole parse, so the result is empty.
pub fn read_client_message_identity(value: &Value) -> ClientMessageIdentity {
    let invalid = |key: &str| value.get(key).is_some() && identity_field(value, key).is_none();
    if !value.is_object() || invalid("clientMessageId") || invalid("clientTurnId") {
        return ClientMessageIdentity::default();
    }
    ClientMessageIdentity {
        client_message_id: identity_field(value, "clientMessageId"),
        client_turn_id: identity_field(value, "clientTurnId"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_valid_identity_and_drops_everything_on_a_bad_field() {
        let ok = read_client_message_identity(
            &json!({"clientMessageId": "m", "clientTurnId": "t", "extra": 1}),
        );
        assert_eq!(
            (
                ok.client_message_id.as_deref(),
                ok.client_turn_id.as_deref()
            ),
            (Some("m"), Some("t"))
        );
        assert_eq!(
            read_client_message_identity(&json!({"clientMessageId": "m", "clientTurnId": ""})),
            ClientMessageIdentity::default()
        );
        assert_eq!(
            read_client_message_identity(&json!({"clientMessageId": "x".repeat(257)})),
            ClientMessageIdentity::default()
        );
        assert_eq!(
            read_client_message_identity(&json!("nope")),
            ClientMessageIdentity::default()
        );
    }

    #[test]
    fn prepared_input_carries_optional_source() {
        let input = PreparedClientInput {
            identity: ClientMessageIdentity::default(),
            text: "t".into(),
            images: None,
            mode: QueueMode::Steer,
            enqueue_order: 1,
            source: Some(InputSource::Rpc),
        };
        assert_eq!(input.source, Some(InputSource::Rpc));
    }
}
