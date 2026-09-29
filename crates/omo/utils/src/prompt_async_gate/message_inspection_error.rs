use serde_json::Value;

pub fn is_prompt_message_inspection_aborted(error: &Value) -> bool {
    if let Some(name) = error.get("name").and_then(Value::as_str)
        && name == "MessageAbortedError"
    {
        return true;
    }
    if let Some(err_str) = error.as_str()
        && err_str.contains("MessageAbortedError")
    {
        return true;
    }
    false
}

pub fn is_prompt_message_inspection_aborted_str(error: &str) -> bool {
    error.contains("MessageAbortedError")
}
