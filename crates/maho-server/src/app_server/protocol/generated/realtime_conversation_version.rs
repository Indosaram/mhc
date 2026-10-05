#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RealtimeConversationVersion {
    #[serde(rename = "v1")]
    V1,
    #[serde(rename = "v2")]
    V2,
    #[serde(rename = "v3")]
    V3,
}
