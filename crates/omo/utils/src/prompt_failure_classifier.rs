//! Classification of prompt dispatch failures that may have reached the server.

use serde_json::Value;

/// Best-effort message: strings verbatim, `.message` of objects, else JSON text.
pub fn extract_prompt_failure_message(error: &Value) -> String {
    match error {
        Value::String(text) => text.clone(),
        Value::Object(object) => match object.get("message") {
            Some(Value::String(message)) => message.clone(),
            _ => error.to_string(),
        },
        Value::Array(_) => error.to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
    }
}

pub fn is_ambiguous_prompt_dispatch_failure(error: &Value) -> bool {
    let message = extract_prompt_failure_message(error).to_lowercase();
    [
        "unexpected eof",
        "json parse error",
        "unexpected end of json input",
        "timed out",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptDispatchFailure {
    pub error: Value,
    pub dispatch_attempted: bool,
}

pub fn is_ambiguous_post_dispatch_prompt_failure(result: &PromptDispatchFailure) -> bool {
    result.dispatch_attempted && is_ambiguous_prompt_dispatch_failure(&result.error)
}
