#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LoginAppBrand {
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "chatgpt")]
    Chatgpt,
}
