#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GuardianApprovalReviewStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "approved")]
    Approved,
    #[serde(rename = "denied")]
    Denied,
    #[serde(rename = "timedOut")]
    TimedOut,
    #[serde(rename = "aborted")]
    Aborted,
}
