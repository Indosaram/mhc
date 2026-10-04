#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ConversationTextRole {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "developer")]
    Developer,
    #[serde(rename = "assistant")]
    Assistant,
}
