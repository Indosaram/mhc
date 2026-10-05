#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitResetCredit {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "resetType")]
    pub reset_type: Box<crate::app_server::protocol::generated::v2::rate_limit_reset_type::RateLimitResetType>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::rate_limit_reset_credit_status::RateLimitResetCreditStatus>,
    #[serde(rename = "grantedAt")]
    pub granted_at: f64,
    #[serde(rename = "expiresAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub expires_at: Option<f64>,
    #[serde(rename = "title", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub title: Option<String>,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
}
