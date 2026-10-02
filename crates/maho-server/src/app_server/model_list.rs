use super::registry::JsonRpcError;
use serde_json::{Value, json};

pub fn build_wire_model(model: &Value, supported_levels: &[String], default_model_id: Option<&str>) -> Value {
    let id = model["id"].as_str().unwrap_or_default();
    let provider = model["provider"].as_str().unwrap_or_default();
    let efforts = if model["reasoning"] == true { supported_levels.iter().filter(|level| *level != "off").map(|level| json!({"reasoningEffort":level,"description":""})).collect::<Vec<_>>() } else { Vec::new() };
    json!({"id":format!("{provider}/{id}"),"model":id,"upgrade":null,"upgradeInfo":null,"availabilityNux":null,"displayName":model["name"].as_str().unwrap_or(id),"description":"","hidden":model["hidden"] == true,"supportedReasoningEfforts":efforts,"defaultReasoningEffort":"medium","inputModalities":["text"],"supportsPersonality":false,"additionalSpeedTiers":[],"serviceTiers":[],"defaultServiceTier":null,"isDefault":default_model_id == Some(id)})
}
pub fn build_model_list_response(wire_models: &[Value], params: &Value) -> Result<Value, JsonRpcError> {
    let models = wire_models.iter().filter(|model| params["includeHidden"] == true || model["hidden"] != true).collect::<Vec<_>>();
    let start = match params.get("cursor").filter(|value| !value.is_null()) {
        None => 0,
        Some(Value::String(cursor)) if !cursor.is_empty() && cursor.bytes().all(|byte| byte.is_ascii_digit()) => cursor.parse::<usize>().ok().filter(|start| *start <= 9_007_199_254_740_991).ok_or_else(|| JsonRpcError::new(-32600, format!("model/list received an invalid cursor: {cursor}")))?,
        Some(cursor) => return Err(JsonRpcError::new(-32600, format!("model/list received an invalid cursor: {cursor}"))),
    };
    if start > models.len() { return Err(JsonRpcError::new(-32600, format!("model/list cursor {start} exceeds total models {}", models.len()))); }
    let limit = params["limit"].as_f64().unwrap_or(models.len() as f64).max(1.0).min(models.len() as f64);
    let end = (start as f64 + limit).min(models.len() as f64) as usize;
    Ok(json!({"data":models[start..end],"nextCursor":if end < models.len() { Some(end.to_string()) } else { None }}))
}
