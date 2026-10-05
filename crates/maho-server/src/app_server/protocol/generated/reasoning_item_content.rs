#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningItemContentReasoningText1 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningItemContentText2 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ReasoningItemContent {
    #[serde(rename = "reasoning_text")]
    ReasoningText(Box<ReasoningItemContentReasoningText1>),
    #[serde(rename = "text")]
    Text(Box<ReasoningItemContentText2>),
}
