#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadHistoryMode {
    #[serde(rename = "legacy")]
    Legacy,
    #[serde(rename = "paginated")]
    Paginated,
}
