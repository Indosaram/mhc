#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitSnapshot {
    #[serde(rename = "limitId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub limit_id: Option<String>,
    #[serde(rename = "limitName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub limit_name: Option<String>,
    #[serde(rename = "primary", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub primary: Option<Box<crate::app_server::protocol::generated::v2::rate_limit_window::RateLimitWindow>>,
    #[serde(rename = "secondary", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub secondary: Option<Box<crate::app_server::protocol::generated::v2::rate_limit_window::RateLimitWindow>>,
    #[serde(rename = "credits", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub credits: Option<Box<crate::app_server::protocol::generated::v2::credits_snapshot::CreditsSnapshot>>,
    #[serde(rename = "individualLimit", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub individual_limit: Option<Box<crate::app_server::protocol::generated::v2::spend_control_limit_snapshot::SpendControlLimitSnapshot>>,
    #[serde(rename = "spendControlReached", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub spend_control_reached: Option<bool>,
    #[serde(rename = "planType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub plan_type: Option<Box<crate::app_server::protocol::generated::plan_type::PlanType>>,
    #[serde(rename = "rateLimitReachedType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub rate_limit_reached_type: Option<Box<crate::app_server::protocol::generated::v2::rate_limit_reached_type::RateLimitReachedType>>,
}
