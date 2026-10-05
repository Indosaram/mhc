#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WindowsSandboxReadiness {
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "notConfigured")]
    NotConfigured,
    #[serde(rename = "updateRequired")]
    UpdateRequired,
}
