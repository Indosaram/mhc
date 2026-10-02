#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileUpdateChange {
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "kind")]
    pub kind: Box<crate::app_server::protocol::generated::v2::patch_change_kind::PatchChangeKind>,
    #[serde(rename = "diff")]
    pub diff: String,
}
