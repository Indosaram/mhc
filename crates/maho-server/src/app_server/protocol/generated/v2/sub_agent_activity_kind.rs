#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubAgentActivityKind {
    #[serde(rename = "started")]
    Started,
    #[serde(rename = "interacted")]
    Interacted,
    #[serde(rename = "interrupted")]
    Interrupted,
}
