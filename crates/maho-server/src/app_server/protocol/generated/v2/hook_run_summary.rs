#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookRunSummary {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "eventName")]
    pub event_name: Box<crate::app_server::protocol::generated::v2::hook_event_name::HookEventName>,
    #[serde(rename = "handlerType")]
    pub handler_type: Box<crate::app_server::protocol::generated::v2::hook_handler_type::HookHandlerType>,
    #[serde(rename = "executionMode")]
    pub execution_mode: Box<crate::app_server::protocol::generated::v2::hook_execution_mode::HookExecutionMode>,
    #[serde(rename = "scope")]
    pub scope: Box<crate::app_server::protocol::generated::v2::hook_scope::HookScope>,
    #[serde(rename = "sourcePath")]
    pub source_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::hook_source::HookSource>,
    #[serde(rename = "displayOrder")]
    pub display_order: i64,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::hook_run_status::HookRunStatus>,
    #[serde(rename = "statusMessage", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub status_message: Option<String>,
    #[serde(rename = "startedAt")]
    pub started_at: i64,
    #[serde(rename = "completedAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub completed_at: Option<i64>,
    #[serde(rename = "durationMs", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub duration_ms: Option<i64>,
    #[serde(rename = "entries")]
    pub entries: Vec<Box<crate::app_server::protocol::generated::v2::hook_output_entry::HookOutputEntry>>,
}
