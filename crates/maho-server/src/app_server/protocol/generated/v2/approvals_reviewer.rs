#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ApprovalsReviewer {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "auto_review")]
    AutoReview,
    #[serde(rename = "guardian_subagent")]
    GuardianSubagent,
}
