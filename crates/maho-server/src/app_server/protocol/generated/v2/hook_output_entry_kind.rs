#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookOutputEntryKind {
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "feedback")]
    Feedback,
    #[serde(rename = "context")]
    Context,
    #[serde(rename = "error")]
    Error,
}
