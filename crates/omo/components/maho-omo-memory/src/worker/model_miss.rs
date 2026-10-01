pub struct ModelMissResult<'a> { pub code: Option<i32>, pub stdout: &'a str, pub stderr: &'a str, pub timed_out: bool }
#[derive(Debug, PartialEq, Eq)]
pub enum RetryableModelMiss { ModelNotVisible { id: String }, AuthMissing { provider: String } }

pub fn classify_retryable_model_miss(result: &ModelMissResult<'_>) -> Option<RetryableModelMiss> {
    if result.timed_out || result.code == Some(0) { return None; }
    let output = format!("{}\n{}", result.stderr, result.stdout);
    for line in output.lines() {
        if let Some(id) = line.strip_prefix("Error: Model \"").and_then(|value| value.strip_suffix("\" not found. Use --list-models to see available models."))
            && !id.is_empty() && !id.contains('"') {
            return Some(RetryableModelMiss::ModelNotVisible { id: id.to_owned() });
        }
    }
    for line in output.lines() {
        let line = line.strip_prefix("Error:").map(str::trim_start).unwrap_or(line);
        if let Some(value) = line.strip_prefix("No API key found for").filter(|value| value.starts_with(char::is_whitespace)) {
            let provider: String = value.trim_start().chars().take_while(|c| !c.is_whitespace() && *c != '.').collect();
            if !provider.is_empty() { return Some(RetryableModelMiss::AuthMissing { provider }); }
        }
    }
    None
}
pub fn is_retryable_model_miss(result: &ModelMissResult<'_>) -> bool { classify_retryable_model_miss(result).is_some() }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_not_visible() { assert_eq!(classify_retryable_model_miss(&ModelMissResult { code: Some(1), stdout: "", stderr: "Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.", timed_out: false }), Some(RetryableModelMiss::ModelNotVisible { id: "extension-only/primary".into() })); }
    #[test]
    fn auth_missing() { assert_eq!(classify_retryable_model_miss(&ModelMissResult { code: Some(1), stdout: "", stderr: "No API key found for anthropic", timed_out: false }), Some(RetryableModelMiss::AuthMissing { provider: "anthropic".into() })); }
    #[test]
    fn timeout_and_success_not_retryable() { for (code, timed_out) in [(Some(1), true), (Some(0), false)] { assert!(classify_retryable_model_miss(&ModelMissResult { code, stdout: "", stderr: "No API key found for anthropic", timed_out }).is_none()); } }
}
