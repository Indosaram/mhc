#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ByteRange {
    #[serde(rename = "start")]
    pub start: f64,
    #[serde(rename = "end")]
    pub end: f64,
}
