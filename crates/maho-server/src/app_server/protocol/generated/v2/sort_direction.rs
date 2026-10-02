#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SortDirection {
    #[serde(rename = "asc")]
    Asc,
    #[serde(rename = "desc")]
    Desc,
}
