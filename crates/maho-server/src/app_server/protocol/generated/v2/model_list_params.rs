#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelListParams {
    #[serde(rename = "cursor", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cursor: Option<Option<String>>,
    #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub limit: Option<Option<f64>>,
    #[serde(rename = "includeHidden", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub include_hidden: Option<Option<bool>>,
}
