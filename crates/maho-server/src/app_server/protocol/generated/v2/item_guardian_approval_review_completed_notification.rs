#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemGuardianApprovalReviewCompletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: f64,
    #[serde(rename = "completedAtMs")]
    pub completed_at_ms: f64,
    #[serde(rename = "reviewId")]
    pub review_id: String,
    #[serde(rename = "targetItemId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub target_item_id: Option<String>,
    #[serde(rename = "decisionSource")]
    pub decision_source: Box<crate::app_server::protocol::generated::v2::auto_review_decision_source::AutoReviewDecisionSource>,
    #[serde(rename = "review")]
    pub review: Box<crate::app_server::protocol::generated::v2::guardian_approval_review::GuardianApprovalReview>,
    #[serde(rename = "action")]
    pub action: Box<crate::app_server::protocol::generated::v2::guardian_approval_review_action::GuardianApprovalReviewAction>,
}
