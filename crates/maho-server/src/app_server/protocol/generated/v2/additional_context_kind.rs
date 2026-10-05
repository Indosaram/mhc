#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AdditionalContextKind {
    #[serde(rename = "untrusted")]
    Untrusted,
    #[serde(rename = "application")]
    Application,
}
