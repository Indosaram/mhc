#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnPlanStep {
    #[serde(rename = "step")]
    pub step: String,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::turn_plan_step_status::TurnPlanStepStatus>,
}
