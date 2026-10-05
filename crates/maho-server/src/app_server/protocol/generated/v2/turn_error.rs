#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnError {
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "codexErrorInfo", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub codex_error_info: Option<Box<crate::app_server::protocol::generated::v2::codex_error_info::CodexErrorInfo>>,
    #[serde(rename = "additionalDetails", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub additional_details: Option<String>,
}
