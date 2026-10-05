#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CodexResponseHandoffMode {
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "commentary")]
    Commentary,
    #[serde(rename = "bemTags")]
    BemTags,
}
