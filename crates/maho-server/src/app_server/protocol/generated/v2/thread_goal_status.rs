#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadGoalStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "usageLimited")]
    UsageLimited,
    #[serde(rename = "budgetLimited")]
    BudgetLimited,
    #[serde(rename = "complete")]
    Complete,
}
