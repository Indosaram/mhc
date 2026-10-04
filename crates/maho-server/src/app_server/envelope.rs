use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub enum ClassifiedIncoming {
    Request(Value),
    Response(Value),
    Notification(Value),
    ProtocolInvalid(Value),
}
fn request_id(value: &Value) -> bool {
    value.is_string() || value.is_number()
}
fn response_id(value: &Value) -> bool {
    value.is_null() || request_id(value)
}
pub fn classify_incoming(value: Value) -> ClassifiedIncoming {
    let Some(object) = value.as_object() else {
        return ClassifiedIncoming::ProtocolInvalid(value);
    };
    if let (Some(id), Some(method)) = (object.get("id"), object.get("method"))
        && request_id(id)
        && method.is_string()
    {
        let mut message = json!({"id":id,"method":method});
        if let Some(params) = object.get("params") {
            message["params"] = params.clone();
        }
        return ClassifiedIncoming::Request(message);
    }
    if let Some(id) = object.get("id")
        && response_id(id)
    {
        if let Some(result) = object.get("result") {
            return ClassifiedIncoming::Response(json!({"id":id,"result":result}));
        }
        if let Some(error) = object.get("error").and_then(Value::as_object)
            && error.get("code").is_some_and(Value::is_number)
            && error.get("message").is_some_and(Value::is_string)
        {
            let mut parsed = json!({"code":error["code"],"message":error["message"]});
            if let Some(data) = error.get("data") {
                parsed["data"] = data.clone();
            }
            return ClassifiedIncoming::Response(json!({"id":id,"error":parsed}));
        }
    }
    if !object.contains_key("id")
        && let Some(method) = object.get("method").filter(|v| v.is_string())
    {
        let mut message = json!({"method":method});
        if let Some(params) = object.get("params") {
            message["params"] = params.clone();
        }
        return ClassifiedIncoming::Notification(message);
    }
    ClassifiedIncoming::ProtocolInvalid(value)
}
pub fn populate_outbound_notification(mut message: Value, emitted_at_ms: u64) -> Value {
    if let Some(object) = message.as_object_mut()
        && object.contains_key("method")
        && !object.contains_key("id")
        && object.get("emittedAtMs").is_none_or(Value::is_null)
    {
        object.insert("emittedAtMs".into(), json!(emitted_at_ms));
    }
    message
}
