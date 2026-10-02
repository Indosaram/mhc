#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ForcedLoginMethod {
    #[serde(rename = "chatgpt")]
    Chatgpt,
    #[serde(rename = "api")]
    Api,
}
