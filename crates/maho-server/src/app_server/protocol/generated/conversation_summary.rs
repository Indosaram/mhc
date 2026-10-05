#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConversationSummary {
    #[serde(rename = "conversationId")]
    pub conversation_id: Box<crate::app_server::protocol::generated::thread_id::ThreadId>,
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "preview")]
    pub preview: String,
    #[serde(rename = "timestamp", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub timestamp: Option<String>,
    #[serde(rename = "updatedAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub updated_at: Option<String>,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "cwd")]
    pub cwd: String,
    #[serde(rename = "cliVersion")]
    pub cli_version: String,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::session_source::SessionSource>,
    #[serde(rename = "gitInfo", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub git_info: Option<Box<crate::app_server::protocol::generated::conversation_git_info::ConversationGitInfo>>,
}
