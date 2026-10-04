#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TurnPlanStepStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
}
