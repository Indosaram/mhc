#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnSteerParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "clientUserMessageId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub client_user_message_id: Option<Option<String>>,
    #[serde(rename = "input")]
    pub input: Vec<Box<crate::app_server::protocol::generated::v2::user_input::UserInput>>,
    #[serde(rename = "expectedTurnId")]
    pub expected_turn_id: String,
}
