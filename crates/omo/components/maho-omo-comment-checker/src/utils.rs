pub fn normalize_feedback_text(value:&str) -> String { value.replace("\r\n","\n").replace('\r',"\n").trim().into() }
pub fn get_string(value:&serde_json::Value) -> Option<&str> { value.as_str() }
pub fn is_record(value:&serde_json::Value) -> bool { value.is_object() }
