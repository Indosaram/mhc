use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{0}")]
pub struct UserInputProtocolError(pub &'static str);
pub fn read_user_input_result(value: &Value) -> Result<Value, UserInputProtocolError> {
    let object = value.as_object().ok_or(UserInputProtocolError("Invalid user input result"))?;
    if object.get("comment").is_some_and(|value| !value.is_string()) { return Err(UserInputProtocolError("Invalid user input comment")); }
    if object.get("cancelled").is_some_and(|value| !value.is_boolean()) { return Err(UserInputProtocolError("Invalid user input cancellation")); }
    let mut answers = serde_json::Map::new();
    if let Some(value) = object.get("answers") {
        let values = value.as_object().ok_or(UserInputProtocolError("Invalid user input answers"))?;
        for (id, answer) in values {
            let strings = answer.get("answers").and_then(Value::as_array).ok_or(UserInputProtocolError("Invalid user input answer"))?;
            if strings.iter().any(|value| !value.is_string()) { return Err(UserInputProtocolError("Invalid user input answer text")); }
            answers.insert(id.clone(), json!({"answers":strings}));
        }
    }
    let mut result = json!({"answers":answers});
    for key in ["comment", "cancelled"] { if let Some(value) = object.get(key) { result[key] = value.clone(); } }
    Ok(result)
}
pub fn to_draft(request: &Value, result: &Value) -> Value {
    let mut answers = serde_json::Map::new();
    for question in request["questions"].as_array().into_iter().flatten() {
        let Some(id) = question["id"].as_str() else { continue; };
        let Some(values) = result["answers"][id]["answers"].as_array() else { continue; };
        let labels = question["options"].as_array().into_iter().flatten().filter_map(|option| option["label"].as_str()).collect::<Vec<_>>();
        let selected = values.iter().filter_map(Value::as_str).filter(|value| labels.contains(value)).collect::<Vec<_>>();
        let text = values.iter().filter_map(Value::as_str).filter(|value| !labels.contains(value)).collect::<Vec<_>>().join("\n");
        let mut answer = json!({"selected":selected});
        if !text.is_empty() { answer["text"] = json!(text); }
        answers.insert(id.into(), answer);
    }
    let mut draft = json!({"answers":answers});
    if let Some(comment) = result.get("comment") { draft["comment"] = comment.clone(); }
    draft
}
