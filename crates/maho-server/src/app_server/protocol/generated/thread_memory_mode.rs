#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadMemoryMode {
    #[serde(rename = "enabled")]
    Enabled,
    #[serde(rename = "disabled")]
    Disabled,
}
