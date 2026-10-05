#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsReadDirectoryEntry {
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "isDirectory")]
    pub is_directory: bool,
    #[serde(rename = "isFile")]
    pub is_file: bool,
}
