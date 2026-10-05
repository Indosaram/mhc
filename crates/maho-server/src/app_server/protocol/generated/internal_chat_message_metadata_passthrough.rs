#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InternalChatMessageMetadataPassthrough {
    #[serde(rename = "turn_id", default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}
