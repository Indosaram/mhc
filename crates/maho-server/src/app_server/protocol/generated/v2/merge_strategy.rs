#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MergeStrategy {
    #[serde(rename = "replace")]
    Replace,
    #[serde(rename = "upsert")]
    Upsert,
}
