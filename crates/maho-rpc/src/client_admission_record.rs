//! The client admission store's record (senpi `client-admission-record.ts`): one
//! `rpc_client_message_admission` entry per client input, its prepared form while queued, and the
//! fingerprint that detects a `clientMessageId` reused for a different input.
use crate::effect_record::EffectOutcome;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CLIENT_ADMISSION_ENTRY: &str = "rpc_client_message_admission";
/// Prefix of a `clientMessageId` the host minted for an id-less queued input; never shown to a client.
pub const HOST_MINTED_ID_PREFIX: &str = "host:";
pub const DURABLE_CLIENT_MESSAGE_ID_CAPABILITY: &str = "durable_client_message_id";

pub fn is_host_minted_id(id: Option<&str>) -> bool {
    id.is_some_and(|id| id.starts_with(HOST_MINTED_ID_PREFIX))
}

/// The identity a client may see: a host-minted `clientMessageId` is internal and dropped.
pub fn client_facing_identity(mut value: serde_json::Value) -> serde_json::Value {
    let minted = is_host_minted_id(
        value
            .get("clientMessageId")
            .and_then(serde_json::Value::as_str),
    );
    if minted && let Some(object) = value.as_object_mut() {
        object.remove("clientMessageId");
    }
    value
}

/// Where a queued input came from. Additive: a record written without it restores with `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreparedInputSource {
    Interactive,
    Rpc,
    Extension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PreparedInputMode {
    Steer,
    FollowUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedInput {
    pub text: String,
    pub mode: PreparedInputMode,
    pub enqueue_order: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PreparedInputSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedImage {
    pub r#type: String,
    pub data: String,
    pub mime_type: String,
}

/// The queued form of an input; carries the client identity beside the text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedAdmissionInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_turn_id: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<PreparedImage>>,
    pub mode: PreparedInputMode,
    pub enqueue_order: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionDisposition {
    Queued,
    Started,
    Handled,
}

/// senpi `clientAdmissionRecordSchema`, discriminated on `state`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum AdmissionState {
    Queued {
        prepared: PreparedAdmissionInput,
    },
    Running,
    /// `outcome` is additive: an older host strips it and still reads the record as completed.
    Completed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<EffectOutcome>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientAdmissionRecord {
    pub client_message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_turn_id: Option<String>,
    pub durable_session_id: String,
    pub fingerprint: String,
    pub disposition: AdmissionDisposition,
    /// The host minted `clientMessageId` for input sent without one (`host:<durableSessionId>:<uuid>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_minted: Option<bool>,
    #[serde(flatten)]
    pub state: AdmissionState,
}

fn bounded_id(id: &str) -> bool {
    (1..=256).contains(&id.chars().count())
}

/// senpi `clientAdmissionRecordSchema.safeParse`.
pub fn parse_client_admission_record(value: &serde_json::Value) -> Option<ClientAdmissionRecord> {
    let record: ClientAdmissionRecord = serde_json::from_value(value.clone()).ok()?;
    let turn_ok = record.client_turn_id.as_deref().is_none_or(bounded_id);
    let minted_ok = record.host_minted.is_none_or(|minted| minted);
    let prepared_ok = match &record.state {
        AdmissionState::Queued { prepared } => {
            prepared.client_message_id.as_deref().is_none_or(bounded_id)
                && prepared.client_turn_id.as_deref().is_none_or(bounded_id)
                && prepared
                    .images
                    .as_ref()
                    .is_none_or(|images| images.iter().all(|image| image.r#type == "image"))
        }
        _ => value.get("prepared").is_none_or(serde_json::Value::is_null),
    };
    (bounded_id(&record.client_message_id) && turn_ok && minted_ok && prepared_ok).then_some(record)
}

impl ClientAdmissionRecord {
    pub fn state_name(&self) -> &'static str {
        match self.state {
            AdmissionState::Queued { .. } => "queued",
            AdmissionState::Running => "running",
            AdmissionState::Completed { .. } => "completed",
        }
    }
}

/// senpi `admissionSnapshot`: the record without `fingerprint` and `prepared`.
pub fn admission_snapshot(record: &ClientAdmissionRecord) -> serde_json::Value {
    let mut value = serde_json::to_value(record).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("fingerprint");
        object.remove("prepared");
    }
    value
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("client_message_id_conflict: {client_message_id}")]
pub struct ClientMessageIdConflict {
    pub client_message_id: String,
}
impl ClientMessageIdConflict {
    pub const CODE: &'static str = "client_message_id_conflict";
}

/// The fingerprint of a `prompt`, `steer` or `follow_up` command (senpi `clientInputFingerprint`). Keys
/// are hashed in upstream order and `undefined` fields are omitted, as `JSON.stringify` does.
pub fn client_input_fingerprint(command: &serde_json::Value) -> String {
    let field = |key: &str| command.get(key).filter(|value| !value.is_null()).cloned();
    let mut payload = serde_json::Map::new();
    let kind = command
        .get("type")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    payload.insert("type".into(), kind.clone());
    if let Some(message) = field("message") {
        payload.insert("message".into(), message);
    }
    let images = command
        .get("images")
        .and_then(serde_json::Value::as_array)
        .map(|images| {
            images
                .iter()
                .map(|image| {
                    let mut out = serde_json::Map::new();
                    for key in ["type", "data", "mimeType"] {
                        if let Some(value) = image.get(key) {
                            out.insert(key.into(), value.clone());
                        }
                    }
                    serde_json::Value::Object(out)
                })
                .collect()
        })
        .unwrap_or_default();
    payload.insert("images".into(), serde_json::Value::Array(images));
    if let Some(turn) = field("clientTurnId") {
        payload.insert("clientTurnId".into(), turn);
    }
    if kind == "prompt" {
        for key in ["streamingBehavior", "thinkingLevel", "sessionTitlePrompt"] {
            if let Some(value) = field(key) {
                payload.insert(key.into(), value);
            }
        }
        payload.insert(
            "expandPromptTemplates".into(),
            field("expandPromptTemplates").unwrap_or(serde_json::Value::Bool(true)),
        );
        payload.insert(
            "unknownCommandAsText".into(),
            field("unknownCommandAsText").unwrap_or(serde_json::Value::Bool(false)),
        );
    }
    hex::encode(Sha256::digest(
        serde_json::Value::Object(payload).to_string().as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_minted_ids_are_hidden_from_clients() {
        assert!(
            is_host_minted_id(Some("host:d:1"))
                && !is_host_minted_id(Some("c1"))
                && !is_host_minted_id(None)
        );
        assert_eq!(
            client_facing_identity(json!({"clientMessageId":"host:d:1","x":1})),
            json!({"x":1})
        );
        assert_eq!(
            client_facing_identity(json!({"clientMessageId":"c1"})),
            json!({"clientMessageId":"c1"})
        );
    }

    #[test]
    fn admission_records_parse_by_state_and_snapshot_drops_internals() {
        let queued = json!({"clientMessageId":"c1","durableSessionId":"d","fingerprint":"f","disposition":"queued","state":"queued","prepared":{"text":"hi","mode":"followUp","enqueueOrder":1}});
        let record = parse_client_admission_record(&queued).expect("queued");
        assert_eq!(record.state_name(), "queued");
        let snapshot = admission_snapshot(&record);
        assert!(snapshot.get("fingerprint").is_none() && snapshot.get("prepared").is_none());
        assert_eq!(snapshot["state"], "queued");
        let completed = json!({"clientMessageId":"c1","durableSessionId":"d","fingerprint":"f","disposition":"started","state":"completed","outcome":{"kind":"cleared"}});
        assert!(parse_client_admission_record(&completed).is_some());
        assert!(parse_client_admission_record(&json!({"clientMessageId":"","durableSessionId":"d","fingerprint":"f","disposition":"queued","state":"running"})).is_none());
        assert!(parse_client_admission_record(&json!({"clientMessageId":"c","durableSessionId":"d","fingerprint":"f","disposition":"queued","state":"running","hostMinted":false})).is_none());
    }

    #[test]
    fn fingerprint_ignores_transport_fields_and_defaults_prompt_flags() {
        let a = client_input_fingerprint(&json!({"type":"prompt","id":"1","message":"m"}));
        let b = client_input_fingerprint(
            &json!({"type":"prompt","id":"2","message":"m","expandPromptTemplates":true,"clientMessageId":"x"}),
        );
        assert_eq!(a, b);
        let expected = hex::encode(Sha256::digest(br#"{"type":"prompt","message":"m","images":[],"expandPromptTemplates":true,"unknownCommandAsText":false}"#));
        assert_eq!(a, expected);
        assert_ne!(
            a,
            client_input_fingerprint(&json!({"type":"steer","message":"m"}))
        );
    }
    #[test]
    fn record_without_source_restores_unknown() {
        let input: PreparedInput =
            serde_json::from_value(json!({"text":"t","mode":"steer","enqueueOrder":1}))
                .expect("legacy record");
        assert_eq!(input.source, None);
        assert!(
            serde_json::to_value(&input)
                .expect("serialize")
                .get("source")
                .is_none()
        );
    }
    #[test]
    fn recorded_source_round_trips_and_unknown_source_is_rejected() {
        let value = json!({"text":"t","mode":"followUp","enqueueOrder":2.0,"source":"extension"});
        let input: PreparedInput = serde_json::from_value(value.clone()).expect("record");
        assert_eq!(input.source, Some(PreparedInputSource::Extension));
        assert_eq!(serde_json::to_value(&input).expect("serialize"), value);
        assert!(
            serde_json::from_value::<PreparedInput>(
                json!({"text":"t","mode":"steer","enqueueOrder":1,"source":"bot"})
            )
            .is_err()
        );
    }
}
