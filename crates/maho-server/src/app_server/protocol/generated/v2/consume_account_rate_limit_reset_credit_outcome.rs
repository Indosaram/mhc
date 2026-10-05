#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ConsumeAccountRateLimitResetCreditOutcome {
    #[serde(rename = "reset")]
    Reset,
    #[serde(rename = "nothingToReset")]
    NothingToReset,
    #[serde(rename = "noCredit")]
    NoCredit,
    #[serde(rename = "alreadyRedeemed")]
    AlreadyRedeemed,
}
