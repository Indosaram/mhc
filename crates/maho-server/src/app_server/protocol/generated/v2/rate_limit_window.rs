#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitWindow {
    #[serde(rename = "usedPercent")]
    pub used_percent: f64,
    #[serde(rename = "windowDurationMins", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub window_duration_mins: Option<f64>,
    #[serde(rename = "resetsAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub resets_at: Option<f64>,
}
