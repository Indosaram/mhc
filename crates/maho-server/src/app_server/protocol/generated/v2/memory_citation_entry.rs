#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryCitationEntry {
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "lineStart")]
    pub line_start: f64,
    #[serde(rename = "lineEnd")]
    pub line_end: f64,
    #[serde(rename = "note")]
    pub note: String,
}
