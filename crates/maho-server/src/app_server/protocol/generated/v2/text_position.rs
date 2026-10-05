#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextPosition {
    #[serde(rename = "line")]
    pub line: f64,
    #[serde(rename = "column")]
    pub column: f64,
}
