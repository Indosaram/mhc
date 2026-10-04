#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatgptAuthTokensRefreshParams {
    #[serde(rename = "reason")]
    pub reason: Box<crate::app_server::protocol::generated::v2::chatgpt_auth_tokens_refresh_reason::ChatgptAuthTokensRefreshReason>,
    #[serde(rename = "previousAccountId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub previous_account_id: Option<Option<String>>,
}
