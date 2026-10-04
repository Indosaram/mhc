pub type LocalShellExecActionEnv1 = std::collections::BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalShellExecAction {
    #[serde(rename = "command")]
    pub command: Vec<String>,
    #[serde(rename = "timeout_ms", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub timeout_ms: Option<i64>,
    #[serde(rename = "working_directory", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub working_directory: Option<String>,
    #[serde(rename = "env", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub env: Option<LocalShellExecActionEnv1>,
    #[serde(rename = "user", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub user: Option<String>,
}
