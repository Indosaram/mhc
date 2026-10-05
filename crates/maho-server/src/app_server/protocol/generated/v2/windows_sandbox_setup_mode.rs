#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WindowsSandboxSetupMode {
    #[serde(rename = "elevated")]
    Elevated,
    #[serde(rename = "unelevated")]
    Unelevated,
}
