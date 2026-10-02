#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchToolConfig {
    #[serde(rename = "context_size", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub context_size: Option<Box<crate::app_server::protocol::generated::web_search_context_size::WebSearchContextSize>>,
    #[serde(rename = "allowed_domains", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(rename = "location", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub location: Option<Box<crate::app_server::protocol::generated::web_search_location::WebSearchLocation>>,
}
