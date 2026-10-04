#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfiguredHookMatcherGroup {
    #[serde(rename = "matcher", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub matcher: Option<String>,
    #[serde(rename = "hooks")]
    pub hooks: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_handler::ConfiguredHookHandler>>,
}
