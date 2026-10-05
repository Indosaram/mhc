#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdditionalContextEntry {
    #[serde(rename = "value")]
    pub value: String,
    #[serde(rename = "kind")]
    pub kind: Box<crate::app_server::protocol::generated::v2::additional_context_kind::AdditionalContextKind>,
}
