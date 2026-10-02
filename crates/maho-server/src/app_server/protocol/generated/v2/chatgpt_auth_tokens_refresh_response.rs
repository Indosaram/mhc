#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatgptAuthTokensRefreshResponse {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "chatgptAccountId")]
    pub chatgpt_account_id: String,
    #[serde(rename = "chatgptPlanType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub chatgpt_plan_type: Option<String>,
}
