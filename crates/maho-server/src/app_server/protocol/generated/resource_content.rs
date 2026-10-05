#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResourceContentVariant01 {
    #[serde(rename = "uri")]
    pub uri: String,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(rename = "text")]
    pub text: String,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResourceContentVariant12 {
    #[serde(rename = "uri")]
    pub uri: String,
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(rename = "blob")]
    pub blob: String,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ResourceContent {
    Variant0(Box<ResourceContentVariant01>),
    Variant1(Box<ResourceContentVariant12>),
}
