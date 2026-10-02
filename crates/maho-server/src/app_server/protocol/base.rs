use serde::{Deserialize,Serialize};
pub type JsonValue=serde_json::Value;
pub type AbsolutePathBuf=String;
pub type LegacyAppPathString=String;
pub type ThreadId=String;
pub type ReasoningEffort=String;
pub type ServiceTier=String;
pub type ThreadSource=String;
pub type ThreadStartSource=String;
pub type ThreadUnsubscribeStatus=String;
pub type TurnEnvironmentParams=JsonValue;
pub type DynamicToolSpec=JsonValue;
pub type SelectedCapabilityRoot=JsonValue;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum RequestId {String(String),Number(serde_json::Number)}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ClientInfo {pub name:String,pub title:Option<String>,pub version:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct InitializeCapabilities {
    pub experimental_api:bool,pub request_attestation:bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub mcp_server_openai_form_elicitation:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]
    pub opt_out_notification_methods:Option<Option<Vec<String>>>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct InitializeParams {pub client_info:ClientInfo,pub capabilities:Option<InitializeCapabilities>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct InitializeResponse {pub user_agent:String,pub codex_home:AbsolutePathBuf,pub platform_family:String,pub platform_os:String}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ApprovalPolicy {#[serde(rename="untrusted")]Untrusted,#[serde(rename="on-request")]OnRequest,#[serde(rename="never")]Never}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct GranularApprovalPolicy {pub sandbox_approval:bool,pub rules:bool,pub skill_approval:bool,pub request_permissions:bool,pub mcp_elicitations:bool}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct GranularApproval {pub granular:GranularApprovalPolicy}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum AskForApproval {Policy(ApprovalPolicy),Granular(GranularApproval)}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ApprovalsReviewer {User,AutoReview,GuardianSubagent}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum SandboxMode {#[serde(rename="read-only")]ReadOnly,#[serde(rename="workspace-write")]WorkspaceWrite,#[serde(rename="danger-full-access")]DangerFullAccess}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum NetworkAccess {Restricted,Enabled}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="camelCase",rename_all_fields="camelCase")]
pub enum SandboxPolicy {
    DangerFullAccess,ReadOnly {network_access:bool},ExternalSandbox {network_access:NetworkAccess},
    WorkspaceWrite {writable_roots:Vec<AbsolutePathBuf>,network_access:bool,exclude_tmpdir_env_var:bool,exclude_slash_tmp:bool},
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum Personality {None,Friendly,Pragmatic}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum MultiAgentModeKind {ExplicitRequestOnly,Proactive}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct CustomMode {pub custom:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum MultiAgentMode {Kind(MultiAgentModeKind),Custom(CustomMode)}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ReasoningSummary {Auto,Concise,Detailed,None}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum ThreadSourceKind {Cli,Vscode,Exec,AppServer,SubAgent,SubAgentReview,SubAgentCompact,SubAgentThreadSpawn,SubAgentOther,Unknown}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SubAgentSource {pub sub_agent:JsonValue}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum SessionSource {Kind(ThreadSourceKind),Custom(CustomMode),SubAgent(SubAgentSource)}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum SortDirection {Asc,Desc}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="camelCase",rename_all_fields="camelCase")]
pub enum ThreadStatus {NotLoaded,Idle,SystemError,Active {active_flags:Vec<JsonValue>}}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct AdditionalContextEntry {pub value:String,pub kind:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="camelCase")]
pub enum UserInput {
    Text {text:String,#[serde(default,skip_serializing_if="Option::is_none")]text_elements:Option<Vec<JsonValue>>},
    Image {#[serde(default,skip_serializing_if="Option::is_none")]detail:Option<String>,url:String},
    LocalImage {#[serde(default,skip_serializing_if="Option::is_none")]detail:Option<String>,path:String},
    Skill {name:String,path:String},Mention {name:String,path:String},
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ActivePermissionProfile {pub id:String,pub extends:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct GitInfo {pub sha:Option<String>,pub branch:Option<String>,pub origin_url:Option<String>}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum RemoteControlConnectionStatus {Disabled,Connecting,Connected,Errored}
