use std::fmt;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeSdkRefusalError { pub content: String, pub category: Option<String> }
impl fmt::Display for ClaudeSdkRefusalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Claude refused this request")?;
        if let Some(category) = self.category.as_deref().filter(|category| !category.is_empty()) { write!(f, " ({category})")?; }
        write!(f, ": {}", self.content)
    }
}
impl std::error::Error for ClaudeSdkRefusalError {}

pub fn refusal_error(message: &Value) -> Option<ClaudeSdkRefusalError> {
    let (content, category) = if message["type"] == "system" && message["subtype"] == "model_refusal_no_fallback" {
        (message["api_refusal_explanation"].as_str().or_else(|| message["content"].as_str()).unwrap_or("undefined"), message["api_refusal_category"].as_str())
    } else if message["type"] == "assistant" && message["message"]["stop_reason"] == "refusal" {
        let details = &message["message"]["stop_details"];
        (details["explanation"].as_str().unwrap_or("The request was blocked by policy."), details["category"].as_str())
    } else { return None; };
    Some(ClaudeSdkRefusalError { content: content.into(), category: category.filter(|category| !category.is_empty()).map(String::from) })
}
pub fn is_claude_sdk_refusal(error: &(dyn std::error::Error + 'static)) -> bool {
    error.is::<ClaudeSdkRefusalError>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn structured_refusal() {
        let error = refusal_error(&json!({"type":"system", "subtype":"model_refusal_no_fallback", "api_refusal_category":"cyber", "api_refusal_explanation":"The request was blocked by policy.", "content":"fallback"})).unwrap();
        assert_eq!(error.category.as_deref(), Some("cyber"));
        assert_eq!(error.to_string(), "Claude refused this request (cyber): The request was blocked by policy.");
        assert!(is_claude_sdk_refusal(&error));
    }
    #[test]
    fn legacy_assistant_refusal() {
        let error = refusal_error(&json!({"type":"assistant", "message":{"stop_reason":"refusal", "stop_details":{"category":"cyber", "explanation":"blocked"}}})).unwrap();
        assert_eq!(error.content, "blocked");
        assert_eq!(error.category.as_deref(), Some("cyber"));
    }
    #[test]
    fn defaults_and_non_refusals() {
        let error = refusal_error(&json!({"type":"assistant", "message":{"stop_reason":"refusal"}})).unwrap();
        assert_eq!(error.content, "The request was blocked by policy.");
        assert!(error.category.is_none());
        assert!(refusal_error(&json!({"type":"assistant", "message":{"stop_reason":"end_turn"}})).is_none());
        assert!(!is_claude_sdk_refusal(&std::io::Error::other("other")));
    }
}
