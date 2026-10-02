#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsInstalledParams {
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub thread_id: Option<Option<String>>,
    #[serde(rename = "forceRefresh", default, skip_serializing_if = "Option::is_none")]
    pub force_refresh: Option<bool>,
}
