#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecResponse {
    #[serde(rename = "exitCode")]
    pub exit_code: f64,
    #[serde(rename = "stdout")]
    pub stdout: String,
    #[serde(rename = "stderr")]
    pub stderr: String,
}
