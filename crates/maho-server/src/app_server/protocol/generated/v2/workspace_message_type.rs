#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WorkspaceMessageType {
    #[serde(rename = "headline")]
    Headline,
    #[serde(rename = "announcement")]
    Announcement,
    #[serde(rename = "unknown")]
    Unknown,
}
