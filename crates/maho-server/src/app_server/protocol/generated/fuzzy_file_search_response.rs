#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FuzzyFileSearchResponse {
    #[serde(rename = "files")]
    pub files: Vec<Box<crate::app_server::protocol::generated::fuzzy_file_search_result::FuzzyFileSearchResult>>,
}
