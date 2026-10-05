#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum InputModality {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "image")]
    Image,
    #[serde(rename = "audio")]
    Audio,
}
