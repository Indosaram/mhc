#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTaskScheduleHourly1 {
    #[serde(rename = "intervalHours")]
    pub interval_hours: f64,
    #[serde(rename = "days", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub days: Option<Vec<Box<crate::app_server::protocol::generated::v2::scheduled_task_weekday::ScheduledTaskWeekday>>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTaskScheduleDaily2 {
    #[serde(rename = "time")]
    pub time: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTaskScheduleWeekdays3 {
    #[serde(rename = "time")]
    pub time: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledTaskScheduleWeekly4 {
    #[serde(rename = "days")]
    pub days: Vec<Box<crate::app_server::protocol::generated::v2::scheduled_task_weekday::ScheduledTaskWeekday>>,
    #[serde(rename = "time")]
    pub time: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ScheduledTaskSchedule {
    #[serde(rename = "hourly")]
    Hourly(Box<ScheduledTaskScheduleHourly1>),
    #[serde(rename = "daily")]
    Daily(Box<ScheduledTaskScheduleDaily2>),
    #[serde(rename = "weekdays")]
    Weekdays(Box<ScheduledTaskScheduleWeekdays3>),
    #[serde(rename = "weekly")]
    Weekly(Box<ScheduledTaskScheduleWeekly4>),
}
