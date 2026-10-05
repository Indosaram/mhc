#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentConnectionNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "environmentId")]
    pub environment_id: String,
}
