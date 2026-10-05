#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ModeKind {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "default")]
    Default,
}
