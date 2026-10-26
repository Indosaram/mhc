//! Stop-interrupted failed tool calls must not imply atomicity or automatic retry.
use maho_agent::types::AfterToolCallResult;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
pub const INTERRUPTED_TOOL_TEXT: &str = "Interrupted by the user's Stop before it finished. Its side effects may be partial. It was not re-run.";
pub fn mark_interrupted(
    stop: Option<&Value>,
    is_error: bool,
    details: &Value,
    override_result: Option<AfterToolCallResult>,
) -> Option<AfterToolCallResult> {
    let Some(stop) = stop.filter(|_| is_error) else {
        return override_result;
    };
    let mut result = override_result.unwrap_or(AfterToolCallResult {
        content: None,
        details: None,
        structured_content: None,
        is_error: None,
        usage: None,
        terminate: None,
    });
    let base = result.details.as_ref().unwrap_or(details);
    let mut merged = base.as_object().cloned().unwrap_or_default();
    merged.insert("interrupted".into(), stop.clone());
    result.details = Some(Value::Object(merged));
    result.content = Some(vec![maho_ai::types::ContentBlock::Text(
        maho_ai::types::TextContent {
            text: INTERRUPTED_TOOL_TEXT.into(),
            audience: None,
            text_signature: None,
        },
    )]);
    Some(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_failed_calls_are_marked() {
        let stop = json!({"pauseSeq":2,"at":"now"});
        assert!(mark_interrupted(Some(&stop), false, &Value::Null, None).is_none());
        let r = mark_interrupted(Some(&stop), true, &json!({"kept":1}), None).expect("interrupted");
        assert_eq!(
            r.details.expect("details"),
            json!({"kept":1,"interrupted":stop})
        );
    }
}
