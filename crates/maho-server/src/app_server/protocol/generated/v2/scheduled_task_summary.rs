#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTaskSummary {
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "prompt")]
    pub prompt: String,
    #[serde(rename = "schedule")]
    pub schedule: Box<crate::app_server::protocol::generated::v2::scheduled_task_schedule::ScheduledTaskSchedule>,
}
