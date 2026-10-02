use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::base::{JsonValue,RemoteControlConnectionStatus};
pub type RemoteControlStatusReadParams=();
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RemoteControlStatusReadResponse {pub status:RemoteControlConnectionStatus,pub server_name:String,pub installation_id:String,pub environment_id:Option<String>}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum RemoteControlClientListOrder {Asc,Desc}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RemoteControlClientListParams {pub environment_id:String,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub order:Option<Option<RemoteControlClientListOrder>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RemoteControlClient {pub client_id:String,pub display_name:Option<String>,pub device_type:Option<String>,pub platform:Option<String>,pub os_version:Option<String>,pub device_model:Option<String>,pub app_version:Option<String>,pub last_seen_at:Option<f64>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RemoteControlClientListResponse {pub data:Vec<RemoteControlClient>,pub next_cursor:Option<String>}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum SkillScope {User,Repo,System,Admin}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SkillInterface {
    #[serde(default,skip_serializing_if="Option::is_none")]pub display_name:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub short_description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub icon_small:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub icon_large:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub brand_color:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub default_prompt:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct SkillToolDependency {
    #[serde(rename="type")]pub kind:String,pub value:String,
    #[serde(default,skip_serializing_if="Option::is_none")]pub description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub transport:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub command:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub url:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct SkillDependencies {pub tools:Vec<SkillToolDependency>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SkillMetadata {
    pub name:String,pub description:String,pub path:String,pub scope:SkillScope,pub enabled:bool,
    #[serde(default,skip_serializing_if="Option::is_none")]pub short_description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub interface:Option<SkillInterface>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub dependencies:Option<SkillDependencies>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct SkillErrorInfo {pub path:String,pub message:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct SkillsListEntry {pub cwd:String,pub skills:Vec<SkillMetadata>,pub errors:Vec<SkillErrorInfo>}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SkillsListParams {
    #[serde(default,skip_serializing_if="Option::is_none")]pub cwds:Option<Vec<String>>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub force_reload:Option<bool>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct SkillsListResponse {pub data:Vec<SkillsListEntry>}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum McpServerStatusDetail {Full,ToolsAndAuthOnly}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum McpAuthStatus {Unsupported,NotLoggedIn,BearerToken,#[serde(rename="oAuth")]OAuth}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpServerInfo {pub name:String,pub title:Option<String>,pub version:String,pub description:Option<String>,pub icons:Option<Vec<JsonValue>>,pub website_url:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Tool {
    pub name:String,pub input_schema:JsonValue,
    #[serde(default,skip_serializing_if="Option::is_none")]pub title:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub output_schema:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub annotations:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub icons:Option<Vec<JsonValue>>,
    #[serde(default,skip_serializing_if="Option::is_none",rename="_meta")]pub meta:Option<JsonValue>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Resource {
    pub name:String,pub uri:String,
    #[serde(default,skip_serializing_if="Option::is_none")]pub annotations:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub mime_type:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub size:Option<f64>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub title:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub icons:Option<Vec<JsonValue>>,
    #[serde(default,skip_serializing_if="Option::is_none",rename="_meta")]pub meta:Option<JsonValue>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ResourceTemplate {
    pub uri_template:String,pub name:String,
    #[serde(default,skip_serializing_if="Option::is_none")]pub annotations:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub title:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]pub mime_type:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpServerStatus {pub name:String,pub server_info:Option<McpServerInfo>,pub tools:BTreeMap<String,Tool>,pub resources:Vec<Resource>,pub resource_templates:Vec<ResourceTemplate>,pub auth_status:McpAuthStatus}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpServerStatusListParams {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub detail:Option<Option<McpServerStatusDetail>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub thread_id:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpServerStatusListResponse {pub data:Vec<McpServerStatus>,pub next_cursor:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct PermissionProfileSummary {pub id:String,pub description:Option<String>,pub allowed:bool}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
pub struct PermissionProfileListParams {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cwd:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PermissionProfileListResponse {pub data:Vec<PermissionProfileSummary>,pub next_cursor:Option<String>}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum ExperimentalFeatureStage {Beta,UnderDevelopment,Stable,Deprecated,Removed}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ExperimentalFeature {pub name:String,pub stage:ExperimentalFeatureStage,pub display_name:Option<String>,pub description:Option<String>,pub announcement:Option<String>,pub enabled:bool,pub default_enabled:bool}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ExperimentalFeatureListParams {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub thread_id:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ExperimentalFeatureListResponse {pub data:Vec<ExperimentalFeature>,pub next_cursor:Option<String>}
