#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HooksListEntry {
    #[serde(rename = "cwd")]
    pub cwd: String,
    #[serde(rename = "hooks")]
    pub hooks: Vec<Box<crate::app_server::protocol::generated::v2::hook_metadata::HookMetadata>>,
    #[serde(rename = "warnings")]
    pub warnings: Vec<String>,
    #[serde(rename = "errors")]
    pub errors: Vec<Box<crate::app_server::protocol::generated::v2::hook_error_info::HookErrorInfo>>,
}
