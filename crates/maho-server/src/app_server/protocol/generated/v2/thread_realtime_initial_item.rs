#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeInitialItem {
    #[serde(rename = "role")]
    pub role: Box<crate::app_server::protocol::generated::conversation_text_role::ConversationTextRole>,
    #[serde(rename = "text")]
    pub text: String,
}
