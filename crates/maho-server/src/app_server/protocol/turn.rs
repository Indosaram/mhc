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
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub client_user_message_id:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub responsesapi_client_metadata:Option<Option<BTreeMap<String,String>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub additional_context:Option<Option<BTreeMap<String,AdditionalContextEntry>>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnStartParams {
    #[serde(flatten)]pub common:TurnCommonParams,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub environments:Option<Option<Vec<TurnEnvironmentParams>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cwd:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub runtime_workspace_roots:Option<Option<Vec<AbsolutePathBuf>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub approval_policy:Option<Option<AskForApproval>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub approvals_reviewer:Option<Option<ApprovalsReviewer>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sandbox_policy:Option<Option<SandboxPolicy>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub permissions:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub model:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub service_tier:Option<Option<ServiceTier>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub effort:Option<Option<ReasoningEffort>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub summary:Option<Option<ReasoningSummary>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub personality:Option<Option<Personality>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub output_schema:Option<Option<JsonValue>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub collaboration_mode:Option<Option<CollaborationMode>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub multi_agent_mode:Option<Option<MultiAgentMode>>,
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
