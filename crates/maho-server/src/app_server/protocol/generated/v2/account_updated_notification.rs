#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountUpdatedNotification {
    #[serde(rename = "authMode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub auth_mode: Option<Box<crate::app_server::protocol::generated::auth_mode::AuthMode>>,
    #[serde(rename = "planType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub plan_type: Option<Box<crate::app_server::protocol::generated::plan_type::PlanType>>,
}
