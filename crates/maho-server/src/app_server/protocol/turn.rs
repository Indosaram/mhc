use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::{base::*,collaboration_mode::CollaborationMode,terminal::{TurnError,TurnStatus}};
pub type TurnsPage=JsonValue;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum TurnItemsView {NotLoaded,Summary,Full}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Turn {pub id:String,pub items:Vec<JsonValue>,pub items_view:TurnItemsView,pub status:TurnStatus,pub error:Option<TurnError>,pub started_at:Option<f64>,pub completed_at:Option<f64>,pub duration_ms:Option<f64>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnCommonParams {
    pub thread_id:ThreadId,pub input:Vec<UserInput>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub client_user_message_id:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub responsesapi_client_metadata:Option<BTreeMap<String,String>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub additional_context:Option<BTreeMap<String,AdditionalContextEntry>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnStartParams {
    #[serde(flatten)]pub common:TurnCommonParams,
    #[serde(default,skip_serializing_if="Option::is_none")]pub environments:Option<Vec<TurnEnvironmentParams>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub cwd:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub runtime_workspace_roots:Option<Vec<AbsolutePathBuf>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub approval_policy:Option<AskForApproval>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub approvals_reviewer:Option<ApprovalsReviewer>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub sandbox_policy:Option<SandboxPolicy>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub permissions:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub model:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub service_tier:Option<ServiceTier>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub effort:Option<ReasoningEffort>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub summary:Option<ReasoningSummary>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub personality:Option<Personality>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub output_schema:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub collaboration_mode:Option<CollaborationMode>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub multi_agent_mode:Option<MultiAgentMode>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct TurnStartResponse {pub turn:Turn}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnSteerParams {#[serde(flatten)]pub common:TurnCommonParams,pub expected_turn_id:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnSteerResponse {pub turn_id:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnInterruptParams {pub thread_id:ThreadId,pub turn_id:String}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct TurnInterruptResponse {}
