#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SleepItem {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: f64,
}
