#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowsWorldWritableWarningNotification {
    #[serde(rename = "samplePaths")]
    pub sample_paths: Vec<String>,
    #[serde(rename = "extraCount")]
    pub extra_count: f64,
    #[serde(rename = "failedScan")]
    pub failed_scan: bool,
}
