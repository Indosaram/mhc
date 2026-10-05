#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RealtimeOutputModality {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "audio")]
    Audio,
}
