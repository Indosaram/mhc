#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CreditsSnapshot {
    #[serde(rename = "hasCredits")]
    pub has_credits: bool,
    #[serde(rename = "unlimited")]
    pub unlimited: bool,
    #[serde(rename = "balance", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub balance: Option<String>,
}
