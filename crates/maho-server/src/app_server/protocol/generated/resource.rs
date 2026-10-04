#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Resource {
    #[serde(rename = "annotations", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub annotations: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "size", default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "uri")]
    pub uri: String,
    #[serde(rename = "icons", default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}
