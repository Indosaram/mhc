#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Thread {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "forkedFromId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub forked_from_id: Option<String>,
    #[serde(rename = "parentThreadId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub parent_thread_id: Option<String>,
    #[serde(rename = "preview")]
    pub preview: String,
    #[serde(rename = "ephemeral")]
    pub ephemeral: bool,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "createdAt")]
    pub created_at: f64,
    #[serde(rename = "updatedAt")]
    pub updated_at: f64,
    #[serde(rename = "recencyAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub recency_at: Option<f64>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::thread_status::ThreadStatus>,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<String>,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "cliVersion")]
    pub cli_version: String,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::session_source::SessionSource>,
    #[serde(rename = "threadSource", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub thread_source: Option<Box<crate::app_server::protocol::generated::v2::thread_source::ThreadSource>>,
    #[serde(rename = "agentNickname", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub agent_nickname: Option<String>,
    #[serde(rename = "agentRole", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub agent_role: Option<String>,
    #[serde(rename = "gitInfo", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub git_info: Option<Box<crate::app_server::protocol::generated::v2::git_info::GitInfo>>,
    #[serde(rename = "name", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub name: Option<String>,
    #[serde(rename = "turns")]
    pub turns: Vec<Box<crate::app_server::protocol::generated::v2::turn::Turn>>,
}
