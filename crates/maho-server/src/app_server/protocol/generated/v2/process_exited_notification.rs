#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProcessExitedNotification {
    #[serde(rename = "processHandle")]
    pub process_handle: String,
    #[serde(rename = "exitCode")]
    pub exit_code: f64,
    #[serde(rename = "stdout")]
    pub stdout: String,
    #[serde(rename = "stdoutCapReached")]
    pub stdout_cap_reached: bool,
    #[serde(rename = "stderr")]
    pub stderr: String,
    #[serde(rename = "stderrCapReached")]
    pub stderr_cap_reached: bool,
}
