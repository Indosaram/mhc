#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FuzzyFileSearchParams {
    #[serde(rename = "query")]
    pub query: String,
    #[serde(rename = "roots")]
    pub roots: Vec<String>,
    #[serde(rename = "cancellationToken", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub cancellation_token: Option<String>,
}
