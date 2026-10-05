use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::{base::*,thread::{Thread,ThreadSortKey},turn::{Turn,TurnItemsView},collaboration_mode::CollaborationMode};
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSearchParams {
    pub search_term:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sort_key:Option<Option<ThreadSortKey>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sort_direction:Option<Option<SortDirection>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub source_kinds:Option<Option<Vec<ThreadSourceKind>>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub archived:Option<Option<bool>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadSearchResult {pub thread:Thread,pub snippet:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSearchResponse {pub data:Vec<ThreadSearchResult>,pub next_cursor:Option<String>,pub backwards_cursor:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSearchOccurrencesParams {
    pub thread_id:String,pub search_term:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadSearchTextRange {pub start:f64,pub end:f64}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSearchOccurrence {pub turn_id:String,pub item_id:String,pub snippet:String,pub snippet_match_range:ThreadSearchTextRange,pub turn_cursor:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSearchOccurrencesResponse {pub data:Vec<ThreadSearchOccurrence>,pub next_cursor:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadTurnsListParams {
    pub thread_id:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sort_direction:Option<Option<SortDirection>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub items_view:Option<Option<TurnItemsView>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadTurnsListResponse {pub data:Vec<Turn>,pub next_cursor:Option<String>,pub backwards_cursor:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadItem {pub id:String,#[serde(rename="type")]pub kind:String,#[serde(flatten)]pub fields:BTreeMap<String,JsonValue>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="lowercase")]
pub enum PatchChangeKind {Add,Delete,Update {move_path:Option<String>}}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct FileUpdateChange {pub path:String,pub kind:PatchChangeKind,pub diff:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadItemEntry {pub turn_id:String,pub item:ThreadItem}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadItemsListParams {
    pub thread_id:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub turn_id:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sort_direction:Option<Option<SortDirection>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadItemsListResponse {pub data:Vec<ThreadItemEntry>,pub next_cursor:Option<String>,pub backwards_cursor:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadCompactStartParams {pub thread_id:String}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct ThreadCompactStartResponse {}
pub type ThreadUnarchiveParams=ThreadCompactStartParams;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadUnarchiveResponse {pub thread:Thread}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum ThreadGoalStatus {Active,Paused,Blocked,UsageLimited,BudgetLimited,Complete}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadGoal {pub thread_id:String,pub objective:String,pub status:ThreadGoalStatus,pub token_budget:Option<f64>,pub tokens_used:f64,pub time_used_seconds:f64,pub created_at:f64,pub updated_at:f64}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadGoalSetParams {
    pub thread_id:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub objective:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub status:Option<Option<ThreadGoalStatus>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub token_budget:Option<Option<f64>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadGoalSetResponse {pub goal:ThreadGoal}
pub type ThreadGoalGetParams=ThreadCompactStartParams;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadGoalGetResponse {pub goal:Option<ThreadGoal>}
pub type ThreadGoalClearParams=ThreadCompactStartParams;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ThreadGoalClearResponse {pub cleared:bool}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSettingsUpdateParams {
    pub thread_id:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cwd:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub approval_policy:Option<Option<AskForApproval>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub approvals_reviewer:Option<Option<ApprovalsReviewer>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sandbox_policy:Option<Option<SandboxPolicy>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub permissions:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub model:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub service_tier:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub effort:Option<Option<ReasoningEffort>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub summary:Option<Option<ReasoningSummary>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub collaboration_mode:Option<Option<CollaborationMode>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub multi_agent_mode:Option<Option<MultiAgentMode>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub personality:Option<Option<Personality>>,
}
pub type ThreadSettingsUpdateResponse=ThreadCompactStartResponse;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSettings {
    pub cwd:String,pub approval_policy:AskForApproval,pub approvals_reviewer:ApprovalsReviewer,pub sandbox_policy:SandboxPolicy,pub active_permission_profile:Option<ActivePermissionProfile>,
    pub model:String,pub model_provider:String,pub service_tier:Option<String>,pub effort:Option<ReasoningEffort>,pub summary:Option<ReasoningSummary>,pub collaboration_mode:CollaborationMode,pub personality:Option<Personality>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub multi_agent_mode:Option<MultiAgentMode>,
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadMetadataGitInfoUpdateParams {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub sha:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub branch:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub origin_url:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadMetadataUpdateParams {pub thread_id:String,#[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub git_info:Option<Option<ThreadMetadataGitInfoUpdateParams>>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ThreadMetadataUpdateResponse {pub thread:Thread}
