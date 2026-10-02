#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum InternalSessionSource {
    #[serde(rename = "memory_consolidation")]
    Value,
}
