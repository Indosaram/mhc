#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAccountResponse {
    #[serde(rename = "account", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub account: Option<Box<crate::app_server::protocol::generated::v2::account::Account>>,
    #[serde(rename = "requiresOpenaiAuth")]
    pub requires_openai_auth: bool,
}
