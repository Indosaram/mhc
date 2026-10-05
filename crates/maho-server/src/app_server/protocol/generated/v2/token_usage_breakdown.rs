#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsageBreakdown {
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: f64,
    #[serde(rename = "cachedInputTokens")]
    pub cached_input_tokens: f64,
    #[serde(rename = "cacheWriteInputTokens")]
    pub cache_write_input_tokens: f64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: f64,
    #[serde(rename = "reasoningOutputTokens")]
    pub reasoning_output_tokens: f64,
}
