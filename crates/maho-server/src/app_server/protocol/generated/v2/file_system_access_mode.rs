#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FileSystemAccessMode {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
    #[serde(rename = "deny")]
    Deny,
}
