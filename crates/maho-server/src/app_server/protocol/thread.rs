use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::{base::*,turn::{Turn,TurnsPage}};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ThreadSortKey {CreatedAt,UpdatedAt,RecencyAt}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Thread {
    pub id:String,pub session_id:String,#[serde(deserialize_with="super::nullable::deserialize_required")]pub forked_from_id:Option<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub parent_thread_id:Option<String>,pub preview:String,pub ephemeral:bool,pub model_provider:String,
    pub created_at:f64,pub updated_at:f64,#[serde(deserialize_with="super::nullable::deserialize_required")]pub recency_at:Option<f64>,pub status:ThreadStatus,#[serde(deserialize_with="super::nullable::deserialize_required")]pub path:Option<String>,pub cwd:AbsolutePathBuf,pub cli_version:String,
    pub source:SessionSource,#[serde(deserialize_with="super::nullable::deserialize_required")]pub thread_source:Option<ThreadSource>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub agent_nickname:Option<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub agent_role:Option<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub git_info:Option<GitInfo>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub name:Option<String>,pub turns:Vec<Turn>,
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadRuntimeOverrides {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub model:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub model_provider:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub service_tier:Option<Option<ServiceTier>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cwd:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub runtime_workspace_roots:Option<Option<Vec<AbsolutePathBuf>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub approval_policy:Option<Option<AskForApproval>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub approvals_reviewer:Option<Option<ApprovalsReviewer>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sandbox:Option<Option<SandboxMode>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub permissions:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub config:Option<Option<BTreeMap<String,JsonValue>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub base_instructions:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub developer_instructions:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadRuntimeResponse {
    pub thread:Thread,pub model:String,pub model_provider:String,#[serde(deserialize_with="super::nullable::deserialize_required")]pub service_tier:Option<String>,pub cwd:AbsolutePathBuf,pub runtime_workspace_roots:Vec<AbsolutePathBuf>,pub instruction_sources:Vec<LegacyAppPathString>,
    pub approval_policy:AskForApproval,pub approvals_reviewer:ApprovalsReviewer,pub sandbox:SandboxPolicy,#[serde(deserialize_with="super::nullable::deserialize_required")]pub active_permission_profile:Option<ActivePermissionProfile>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub reasoning_effort:Option<ReasoningEffort>,pub multi_agent_mode:MultiAgentMode,
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadStartParams {
    #[serde(flatten)]pub overrides:ThreadRuntimeOverrides,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub service_name:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub personality:Option<Option<Personality>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub multi_agent_mode:Option<Option<MultiAgentMode>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub ephemeral:Option<Option<bool>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub session_start_source:Option<Option<ThreadStartSource>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub thread_source:Option<Option<ThreadSource>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub environments:Option<Option<Vec<TurnEnvironmentParams>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub dynamic_tools:Option<Option<Vec<DynamicToolSpec>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub selected_capability_roots:Option<Option<Vec<SelectedCapabilityRoot>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub mock_experimental_field:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub experimental_raw_events:Option<bool>,
}
pub type ThreadStartResponse=ThreadRuntimeResponse;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadResumeParams {
    #[serde(flatten)]pub overrides:ThreadRuntimeOverrides,pub thread_id:ThreadId,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub history:Option<Option<Vec<JsonValue>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub path:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub personality:Option<Option<Personality>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub exclude_turns:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub initial_turns_page:Option<Option<JsonValue>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadResumeResponse {#[serde(flatten)]pub runtime:ThreadRuntimeResponse,#[serde(deserialize_with="super::nullable::deserialize_required")]pub initial_turns_page:Option<TurnsPage>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadForkParams {
    #[serde(flatten)]pub overrides:ThreadRuntimeOverrides,pub thread_id:ThreadId,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub path:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub ephemeral:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub thread_source:Option<Option<ThreadSource>>,
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
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sort_key:Option<Option<ThreadSortKey>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sort_direction:Option<Option<SortDirection>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub model_providers:Option<Option<Vec<String>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub source_kinds:Option<Option<Vec<ThreadSourceKind>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub archived:Option<Option<bool>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cwd:Option<Option<CwdFilter>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub use_state_db_only:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub search_term:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub parent_thread_id:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadListResponse {pub data:Vec<Thread>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub next_cursor:Option<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub backwards_cursor:Option<String>}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadLoadedListParams {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadLoadedListResponse {pub data:Vec<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub next_cursor:Option<String>}
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
