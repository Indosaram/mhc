#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InitializeResponse {
    #[serde(rename = "userAgent")]
    pub user_agent: String,
    #[serde(rename = "codexHome")]
    pub codex_home: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "platformFamily")]
    pub platform_family: String,
    #[serde(rename = "platformOs")]
    pub platform_os: String,
}
