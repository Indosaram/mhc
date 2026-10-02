#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalSetParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "objective", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub objective: Option<Option<String>>,
    #[serde(rename = "status", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub status: Option<Option<Box<crate::app_server::protocol::generated::v2::thread_goal_status::ThreadGoalStatus>>>,
    #[serde(rename = "tokenBudget", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub token_budget: Option<Option<f64>>,
}
