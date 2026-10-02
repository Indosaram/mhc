#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowsSandboxSetupStartResponse {
    #[serde(rename = "started")]
    pub started: bool,
}
