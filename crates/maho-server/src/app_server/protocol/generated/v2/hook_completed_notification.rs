#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookCompletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub turn_id: Option<String>,
    #[serde(rename = "run")]
    pub run: Box<crate::app_server::protocol::generated::v2::hook_run_summary::HookRunSummary>,
}
