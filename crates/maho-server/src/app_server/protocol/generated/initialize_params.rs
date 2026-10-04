#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "clientInfo")]
    pub client_info: Box<crate::app_server::protocol::generated::client_info::ClientInfo>,
    #[serde(rename = "capabilities", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub capabilities: Option<Box<crate::app_server::protocol::generated::initialize_capabilities::InitializeCapabilities>>,
}
