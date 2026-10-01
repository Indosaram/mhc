use crate::definition::ToolContent;
pub fn model_only_text(text: impl Into<String>) -> ToolContent {
    ToolContent::Text { text: text.into(), audience: Some("model".into()) }
}
pub fn is_model_only_text(part: &ToolContent) -> bool {
    matches!(part, ToolContent::Text { audience: Some(audience), .. } if audience == "model")
}
