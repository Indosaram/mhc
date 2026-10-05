#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FuzzyFileSearchMatchType {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "directory")]
    Directory,
}
