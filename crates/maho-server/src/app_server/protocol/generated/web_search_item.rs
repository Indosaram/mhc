#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchItem {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "query")]
    pub query: String,
    #[serde(rename = "action", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub action: Option<Box<crate::app_server::protocol::generated::v2::web_search_action::WebSearchAction>>,
    #[serde(rename = "results", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub results: Option<Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>>,
}
