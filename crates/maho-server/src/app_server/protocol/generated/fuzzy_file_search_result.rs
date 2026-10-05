#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FuzzyFileSearchResult {
    #[serde(rename = "root")]
    pub root: String,
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "match_type")]
    pub match_type: Box<crate::app_server::protocol::generated::fuzzy_file_search_match_type::FuzzyFileSearchMatchType>,
    #[serde(rename = "file_name")]
    pub file_name: String,
    #[serde(rename = "score")]
    pub score: f64,
    #[serde(rename = "indices", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub indices: Option<Vec<f64>>,
}
