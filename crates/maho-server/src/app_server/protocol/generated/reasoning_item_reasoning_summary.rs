#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReasoningItemReasoningSummaryType1 {
    #[serde(rename = "summary_text")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningItemReasoningSummary {
    #[serde(rename = "type")]
    pub r#type: ReasoningItemReasoningSummaryType1,
    #[serde(rename = "text")]
    pub text: String,
}
