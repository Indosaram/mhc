#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountTokenUsageDailyBucket {
    #[serde(rename = "startDate")]
    pub start_date: String,
    #[serde(rename = "tokens")]
    pub tokens: i64,
}
