#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResourceTemplate {
    #[serde(rename = "annotations", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub annotations: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}
