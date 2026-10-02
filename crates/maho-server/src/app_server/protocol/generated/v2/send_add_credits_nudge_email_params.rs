#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SendAddCreditsNudgeEmailParams {
    #[serde(rename = "creditType")]
    pub credit_type: Box<crate::app_server::protocol::generated::v2::add_credits_nudge_credit_type::AddCreditsNudgeCreditType>,
}
