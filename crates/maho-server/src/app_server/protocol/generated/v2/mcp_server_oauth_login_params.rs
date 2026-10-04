#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerOauthLoginParams {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub thread_id: Option<Option<String>>,
    #[serde(rename = "scopes", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub scopes: Option<Option<Vec<String>>>,
    #[serde(rename = "timeoutSecs", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub timeout_secs: Option<Option<i64>>,
}
