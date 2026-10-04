#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReview {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::guardian_approval_review_status::GuardianApprovalReviewStatus>,
    #[serde(rename = "riskLevel", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub risk_level: Option<Box<crate::app_server::protocol::generated::v2::guardian_risk_level::GuardianRiskLevel>>,
    #[serde(rename = "userAuthorization", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub user_authorization: Option<Box<crate::app_server::protocol::generated::v2::guardian_user_authorization::GuardianUserAuthorization>>,
    #[serde(rename = "rationale", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub rationale: Option<String>,
}
