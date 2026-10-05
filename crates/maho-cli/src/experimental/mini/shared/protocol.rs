use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef { pub provider: String, pub model_id: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary { pub provider: String, pub model_id: String, pub name: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType { Oauth, ApiKey }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccount {
    pub id: String, pub name: String, pub auth_type: AuthType, pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>, pub interactive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method_name: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelsState { pub models: Vec<ModelSummary>, pub accounts: Vec<ProviderAccount>, pub refreshing: bool }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary { pub id: String, pub path: String, pub cwd: String, pub created_at: f64 }
#[derive(Clone, Copy)]
pub struct ServiceToken { pub name: &'static str }
pub const fn define_service(name: &'static str) -> ServiceToken { ServiceToken { name } }
pub const LANE: ServiceToken = define_service("lane");
pub const MODELS: ServiceToken = define_service("models");
pub const WORKER: ServiceToken = define_service("worker");
pub const SESSIONS: ServiceToken = define_service("sessions");
#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult { Ok, Error(String) }
impl Serialize for CommandResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self { Self::Ok => serde_json::json!({"ok":true}).serialize(serializer), Self::Error(error) => serde_json::json!({"ok":false, "error":error}).serialize(serializer) }
    }
}
impl<'de> Deserialize<'de> for CommandResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value.get("ok").and_then(serde_json::Value::as_bool) {
            Some(true) => Ok(Self::Ok),
            Some(false) => value.get("error").and_then(serde_json::Value::as_str).map(|error| Self::Error(error.to_owned())).ok_or_else(|| serde::de::Error::custom("Failed command requires an error string")),
            None => Err(serde::de::Error::custom("Command result requires an ok boolean")),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthPromptOption { pub id: String, pub label: String, #[serde(skip_serializing_if = "Option::is_none")] pub description: Option<String> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthPromptRequest {
    Text { message: String, #[serde(skip_serializing_if = "Option::is_none")] placeholder: Option<String> },
    Secret { message: String, #[serde(skip_serializing_if = "Option::is_none")] placeholder: Option<String> },
    Select { message: String, options: Vec<AuthPromptOption> },
    ManualCode { message: String, #[serde(skip_serializing_if = "Option::is_none")] placeholder: Option<String> },
}
impl From<maho_ai::auth::types::AuthPrompt> for AuthPromptRequest {
    fn from(prompt: maho_ai::auth::types::AuthPrompt) -> Self {
        use maho_ai::auth::types::AuthPromptKind;
        match prompt.kind {
            AuthPromptKind::Text { message, placeholder } => Self::Text { message, placeholder },
            AuthPromptKind::Secret { message, placeholder } => Self::Secret { message, placeholder },
            AuthPromptKind::ManualCode { message, placeholder } => Self::ManualCode { message, placeholder },
            AuthPromptKind::Select { message, options } => Self::Select { message, options: options.into_iter().map(|option| AuthPromptOption { id: option.id, label: option.label, description: option.description }).collect() },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthInfoLink { pub url: String, #[serde(skip_serializing_if = "Option::is_none")] pub label: Option<String> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthNotice {
    Info { message: String, #[serde(skip_serializing_if = "Option::is_none")] links: Option<Vec<AuthInfoLink>> },
    AuthUrl { url: String, #[serde(skip_serializing_if = "Option::is_none")] instructions: Option<String> },
    DeviceCode {
        #[serde(rename = "userCode")] user_code: String,
        #[serde(rename = "verificationUri")] verification_uri: String,
        #[serde(rename = "intervalSeconds", skip_serializing_if = "Option::is_none")] interval_seconds: Option<f64>,
        #[serde(rename = "expiresInSeconds", skip_serializing_if = "Option::is_none")] expires_in_seconds: Option<f64>,
    },
    Progress { message: String },
}
impl From<maho_ai::auth::types::AuthEvent> for AuthNotice {
    fn from(event: maho_ai::auth::types::AuthEvent) -> Self {
        use maho_ai::auth::types::AuthEvent;
        match event {
            AuthEvent::Info { message, links } => Self::Info { message, links: links.map(|links| links.into_iter().map(|link| AuthInfoLink { url: link.url, label: link.label }).collect()) },
            AuthEvent::AuthUrl { url, instructions } => Self::AuthUrl { url, instructions },
            AuthEvent::DeviceCode { user_code, verification_uri, interval_seconds, expires_in_seconds } => Self::DeviceCode { user_code, verification_uri, interval_seconds, expires_in_seconds },
            AuthEvent::Progress { message } => Self::Progress { message },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelsEvent {
    State { state: ModelsState },
    Prompt { #[serde(rename = "requestId")] request_id: String, request: AuthPromptRequest },
    Notice { notice: AuthNotice },
}
/// The login half of `ModelsEvent`, for whatever drives the dialog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEventPayload {
    Prompt { #[serde(rename = "requestId")] request_id: String, request: AuthPromptRequest },
    Notice { notice: AuthNotice },
}
impl AuthEventPayload {
    /// `undefined` when the event is `Models` state, which is not part of the login half.
    pub fn from_models_event(event: &ModelsEvent) -> Option<Self> {
        match event {
            ModelsEvent::State { .. } => None,
            ModelsEvent::Prompt { request_id, request } => Some(Self::Prompt { request_id: request_id.clone(), request: request.clone() }),
            ModelsEvent::Notice { notice } => Some(Self::Notice { notice: notice.clone() }),
        }
    }
}
/// Everything the `Models` service publishes, as it travels: state and the login half.
impl From<ModelsEvent> for serde_json::Value {
    fn from(event: ModelsEvent) -> Self {
        serde_json::to_value(event).expect("ModelsEvent serializes")
    }
}
/// One presentation's subscription: a `lane.watch()` in the worker, named so its events can be filtered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneSubscription {
    pub subscription_id: String,
    pub snapshot: SessionSnapshot,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub cwd: String,
    pub session_path: String,
    /// Carries the lane configuration, queues, and stats: no side-channel replication.
    pub lane: serde_json::Value,
    pub models: ModelsState,
}
/// Lane events are addressed to the subscription whose watch produced them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneEvent {
    pub subscription_id: String,
    pub event: serde_json::Value,
}
/// Durable session identity the worker describes to the server without naming lane methods.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerDescription {
    pub session_id: String,
}
