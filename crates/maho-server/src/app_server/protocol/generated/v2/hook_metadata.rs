#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookMetadata {
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "eventName")]
    pub event_name: Box<crate::app_server::protocol::generated::v2::hook_event_name::HookEventName>,
    #[serde(rename = "handlerType")]
    pub handler_type: Box<crate::app_server::protocol::generated::v2::hook_handler_type::HookHandlerType>,
    #[serde(rename = "matcher", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub matcher: Option<String>,
    #[serde(rename = "command", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub command: Option<String>,
    #[serde(rename = "timeoutSec")]
    pub timeout_sec: i64,
    #[serde(rename = "statusMessage", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub status_message: Option<String>,
    #[serde(rename = "additionalContextLimit", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub additional_context_limit: Option<f64>,
    #[serde(rename = "sourcePath")]
    pub source_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::hook_source::HookSource>,
    #[serde(rename = "pluginId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub plugin_id: Option<String>,
    #[serde(rename = "displayOrder")]
    pub display_order: i64,
    #[serde(rename = "enabled")]
    pub enabled: bool,
    #[serde(rename = "isManaged")]
    pub is_managed: bool,
    #[serde(rename = "currentHash")]
    pub current_hash: String,
    #[serde(rename = "trustStatus")]
    pub trust_status: Box<crate::app_server::protocol::generated::v2::hook_trust_status::HookTrustStatus>,
}
