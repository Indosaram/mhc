use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::{base::*,turn::{Turn,TurnsPage}};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ThreadSortKey {CreatedAt,UpdatedAt,RecencyAt}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Thread {
    pub id:String,pub session_id:String,pub forked_from_id:Option<String>,pub parent_thread_id:Option<String>,pub preview:String,pub ephemeral:bool,pub model_provider:String,
    pub created_at:f64,pub updated_at:f64,pub recency_at:Option<f64>,pub status:ThreadStatus,pub path:Option<String>,pub cwd:AbsolutePathBuf,pub cli_version:String,
    pub source:SessionSource,pub thread_source:Option<ThreadSource>,pub agent_nickname:Option<String>,pub agent_role:Option<String>,pub git_info:Option<GitInfo>,pub name:Option<String>,pub turns:Vec<Turn>,
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadRuntimeOverrides {
    #[serde(default,skip_serializing_if="Option::is_none")]pub model:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub model_provider:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub service_tier:Option<ServiceTier>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub cwd:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub runtime_workspace_roots:Option<Vec<AbsolutePathBuf>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub approval_policy:Option<AskForApproval>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub approvals_reviewer:Option<ApprovalsReviewer>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub sandbox:Option<SandboxMode>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub permissions:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub config:Option<BTreeMap<String,JsonValue>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub base_instructions:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub developer_instructions:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadRuntimeResponse {
    pub thread:Thread,pub model:String,pub model_provider:String,pub service_tier:Option<String>,pub cwd:AbsolutePathBuf,pub runtime_workspace_roots:Vec<AbsolutePathBuf>,pub instruction_sources:Vec<LegacyAppPathString>,
    pub approval_policy:AskForApproval,pub approvals_reviewer:ApprovalsReviewer,pub sandbox:SandboxPolicy,pub active_permission_profile:Option<ActivePermissionProfile>,pub reasoning_effort:Option<ReasoningEffort>,pub multi_agent_mode:MultiAgentMode,
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadStartParams {
    #[serde(flatten)]pub overrides:ThreadRuntimeOverrides,
    #[serde(default,skip_serializing_if="Option::is_none")]pub service_name:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub personality:Option<Personality>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub multi_agent_mode:Option<MultiAgentMode>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub ephemeral:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub session_start_source:Option<ThreadStartSource>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub thread_source:Option<ThreadSource>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub environments:Option<Vec<TurnEnvironmentParams>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub dynamic_tools:Option<Vec<DynamicToolSpec>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub selected_capability_roots:Option<Vec<SelectedCapabilityRoot>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub mock_experimental_field:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub experimental_raw_events:Option<bool>,
}
pub type ThreadStartResponse=ThreadRuntimeResponse;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadResumeParams {
    #[serde(flatten)]pub overrides:ThreadRuntimeOverrides,pub thread_id:ThreadId,
    #[serde(default,skip_serializing_if="Option::is_none")]pub history:Option<Vec<JsonValue>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub path:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub personality:Option<Personality>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub exclude_turns:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub initial_turns_page:Option<JsonValue>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadResumeResponse {#[serde(flatten)]pub runtime:ThreadRuntimeResponse,pub initial_turns_page:Option<TurnsPage>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadForkParams {
    #[serde(flatten)]pub overrides:ThreadRuntimeOverrides,pub thread_id:ThreadId,
    #[serde(default,skip_serializing_if="Option::is_none")]pub path:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub ephemeral:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub thread_source:Option<ThreadSource>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub exclude_turns:Option<bool>,
}
pub type ThreadForkResponse=ThreadRuntimeResponse;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadReadParams {pub thread_id:ThreadId,#[serde(default,skip_serializing_if="Option::is_none")]pub include_turns:Option<bool>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadReadResponse {pub thread:Thread}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum CwdFilter {One(String),Many(Vec<String>)}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadListParams {
    #[serde(default,skip_serializing_if="Option::is_none")]pub cursor:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub limit:Option<f64>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub sort_key:Option<ThreadSortKey>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub sort_direction:Option<SortDirection>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub model_providers:Option<Vec<String>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub source_kinds:Option<Vec<ThreadSourceKind>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub archived:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub cwd:Option<CwdFilter>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub use_state_db_only:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub search_term:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub parent_thread_id:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadListResponse {pub data:Vec<Thread>,pub next_cursor:Option<String>,pub backwards_cursor:Option<String>}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadLoadedListParams {
    #[serde(default,skip_serializing_if="Option::is_none")]pub cursor:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub limit:Option<f64>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadLoadedListResponse {pub data:Vec<String>,pub next_cursor:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSetNameParams {pub thread_id:ThreadId,pub name:String}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct ThreadSetNameResponse {}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadArchiveParams {pub thread_id:ThreadId}
pub type ThreadArchiveResponse=ThreadSetNameResponse;
pub type ThreadDeleteParams=ThreadArchiveParams;
pub type ThreadDeleteResponse=ThreadSetNameResponse;
pub type ThreadUnsubscribeParams=ThreadArchiveParams;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ThreadUnsubscribeResponse {pub status:ThreadUnsubscribeStatus}
