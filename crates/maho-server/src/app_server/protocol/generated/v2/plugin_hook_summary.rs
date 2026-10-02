#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginHookSummary {
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "eventName")]
    pub event_name: Box<crate::app_server::protocol::generated::v2::hook_event_name::HookEventName>,
}
