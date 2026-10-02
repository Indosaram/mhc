use serde::{Deserialize,Serialize};

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum NonSteerableTurnKind {Review,Compact}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum CodexErrorKind {ContextWindowExceeded,SessionBudgetExceeded,UsageLimitExceeded,ServerOverloaded,CyberPolicy,InternalServerError,Unauthorized,BadRequest,ThreadRollbackFailed,SandboxError,Other}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct HttpConnectionError {#[serde(deserialize_with="super::nullable::deserialize_required")]pub http_status_code:Option<f64>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ActiveTurnNotSteerable {pub turn_kind:NonSteerableTurnKind}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum CodexErrorDetails {
    HttpConnectionFailed(HttpConnectionError),ResponseStreamConnectionFailed(HttpConnectionError),
    ResponseStreamDisconnected(HttpConnectionError),ResponseTooManyFailedAttempts(HttpConnectionError),
    ActiveTurnNotSteerable(ActiveTurnNotSteerable),
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum CodexErrorInfo {Kind(CodexErrorKind),Details(CodexErrorDetails)}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum TurnStatus {Completed,Interrupted,Failed,InProgress}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnError {pub message:String,#[serde(deserialize_with="super::nullable::deserialize_required")]pub codex_error_info:Option<CodexErrorInfo>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub additional_details:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ErrorNotification {pub error:TurnError,pub will_retry:bool,pub thread_id:String,pub turn_id:String}
