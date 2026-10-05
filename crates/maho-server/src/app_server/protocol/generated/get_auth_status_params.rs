#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAuthStatusParams {
    #[serde(rename = "includeToken", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub include_token: Option<bool>,
    #[serde(rename = "refreshToken", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub refresh_token: Option<bool>,
}
