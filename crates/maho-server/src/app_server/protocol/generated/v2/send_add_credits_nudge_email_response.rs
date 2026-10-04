#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SendAddCreditsNudgeEmailResponse {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::add_credits_nudge_email_status::AddCreditsNudgeEmailStatus>,
}
