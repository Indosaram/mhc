#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExperimentalFeatureListResponse {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::experimental_feature::ExperimentalFeature>>,
    #[serde(rename = "nextCursor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub next_cursor: Option<String>,
}
