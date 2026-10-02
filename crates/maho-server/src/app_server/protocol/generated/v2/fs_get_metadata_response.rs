#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsGetMetadataResponse {
    #[serde(rename = "isDirectory")]
    pub is_directory: bool,
    #[serde(rename = "isFile")]
    pub is_file: bool,
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: f64,
    #[serde(rename = "modifiedAtMs")]
    pub modified_at_ms: f64,
}
