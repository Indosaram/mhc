#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AddCreditsNudgeEmailStatus {
    #[serde(rename = "sent")]
    Sent,
    #[serde(rename = "cooldown_active")]
    CooldownActive,
}
