#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookTrustStatus {
    #[serde(rename = "managed")]
    Managed,
    #[serde(rename = "untrusted")]
    Untrusted,
    #[serde(rename = "trusted")]
    Trusted,
    #[serde(rename = "modified")]
    Modified,
}
