#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant01 {
    #[serde(rename = "contextWindowExceeded")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant12 {
    #[serde(rename = "sessionBudgetExceeded")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant23 {
    #[serde(rename = "usageLimitExceeded")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant34 {
    #[serde(rename = "serverOverloaded")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant45 {
    #[serde(rename = "cyberPolicy")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant56HttpConnectionFailed7 {
    #[serde(rename = "httpStatusCode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub http_status_code: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant56 {
    #[serde(rename = "httpConnectionFailed")]
    pub http_connection_failed: CodexErrorInfoVariant56HttpConnectionFailed7,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant68ResponseStreamConnectionFailed9 {
    #[serde(rename = "httpStatusCode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub http_status_code: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant68 {
    #[serde(rename = "responseStreamConnectionFailed")]
    pub response_stream_connection_failed: CodexErrorInfoVariant68ResponseStreamConnectionFailed9,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant710 {
    #[serde(rename = "internalServerError")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant811 {
    #[serde(rename = "unauthorized")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant912 {
    #[serde(rename = "badRequest")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant1013 {
    #[serde(rename = "threadRollbackFailed")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant1114 {
    #[serde(rename = "sandboxError")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant1215ResponseStreamDisconnected16 {
    #[serde(rename = "httpStatusCode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub http_status_code: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant1215 {
    #[serde(rename = "responseStreamDisconnected")]
    pub response_stream_disconnected: CodexErrorInfoVariant1215ResponseStreamDisconnected16,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant1317ResponseTooManyFailedAttempts18 {
    #[serde(rename = "httpStatusCode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub http_status_code: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant1317 {
    #[serde(rename = "responseTooManyFailedAttempts")]
    pub response_too_many_failed_attempts: CodexErrorInfoVariant1317ResponseTooManyFailedAttempts18,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant1419ActiveTurnNotSteerable20 {
    #[serde(rename = "turnKind")]
    pub turn_kind: Box<crate::app_server::protocol::generated::v2::non_steerable_turn_kind::NonSteerableTurnKind>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexErrorInfoVariant1419 {
    #[serde(rename = "activeTurnNotSteerable")]
    pub active_turn_not_steerable: CodexErrorInfoVariant1419ActiveTurnNotSteerable20,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexErrorInfoVariant1521 {
    #[serde(rename = "other")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CodexErrorInfo {
    Variant0(Box<CodexErrorInfoVariant01>),
    Variant1(Box<CodexErrorInfoVariant12>),
    Variant2(Box<CodexErrorInfoVariant23>),
    Variant3(Box<CodexErrorInfoVariant34>),
    Variant4(Box<CodexErrorInfoVariant45>),
    Variant5(Box<CodexErrorInfoVariant56>),
    Variant6(Box<CodexErrorInfoVariant68>),
    Variant7(Box<CodexErrorInfoVariant710>),
    Variant8(Box<CodexErrorInfoVariant811>),
    Variant9(Box<CodexErrorInfoVariant912>),
    Variant10(Box<CodexErrorInfoVariant1013>),
    Variant11(Box<CodexErrorInfoVariant1114>),
    Variant12(Box<CodexErrorInfoVariant1215>),
    Variant13(Box<CodexErrorInfoVariant1317>),
    Variant14(Box<CodexErrorInfoVariant1419>),
    Variant15(Box<CodexErrorInfoVariant1521>),
}
