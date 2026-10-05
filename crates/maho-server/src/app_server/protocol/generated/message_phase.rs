#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MessagePhase {
    #[serde(rename = "commentary")]
    Commentary,
    #[serde(rename = "final_answer")]
    FinalAnswer,
}
