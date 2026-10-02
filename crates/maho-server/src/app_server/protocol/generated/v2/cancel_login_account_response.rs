#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelLoginAccountResponse {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::cancel_login_account_status::CancelLoginAccountStatus>,
}
