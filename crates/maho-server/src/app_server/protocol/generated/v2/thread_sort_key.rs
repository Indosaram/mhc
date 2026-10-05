#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadSortKey {
    #[serde(rename = "created_at")]
    CreatedAt,
    #[serde(rename = "updated_at")]
    UpdatedAt,
    #[serde(rename = "recency_at")]
    RecencyAt,
}
