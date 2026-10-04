pub type FeedbackUploadParamsTags1 = std::collections::BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FeedbackUploadParams {
    #[serde(rename = "classification")]
    pub classification: String,
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub reason: Option<Option<String>>,
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub thread_id: Option<Option<String>>,
    #[serde(rename = "includeLogs", default, skip_serializing_if = "Option::is_none")]
    pub include_logs: Option<bool>,
    #[serde(rename = "extraLogFiles", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub extra_log_files: Option<Option<Vec<String>>>,
    #[serde(rename = "tags", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub tags: Option<Option<FeedbackUploadParamsTags1>>,
}
