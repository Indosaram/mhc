#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigWarningNotification {
    #[serde(rename = "summary")]
    pub summary: String,
    #[serde(rename = "details", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub details: Option<String>,
    #[serde(rename = "path", default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "range", default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Box<crate::app_server::protocol::generated::v2::text_range::TextRange>>,
}
