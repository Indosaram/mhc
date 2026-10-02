#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceAddParams {
    #[serde(rename = "source")]
    pub source: String,
    #[serde(rename = "refName", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub ref_name: Option<Option<String>>,
    #[serde(rename = "sparsePaths", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sparse_paths: Option<Option<Vec<String>>>,
}
