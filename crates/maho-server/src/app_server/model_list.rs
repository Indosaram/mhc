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
    let total = models.len();
    let start = parse_model_list_cursor(params.get("cursor"), total)?;
    let limit = normalize_model_list_limit(params.get("limit"), total);
    // Pinned computes `end = Math.min(start + limit, total)` as a JavaScript number; `slice` then
    // truncates toward zero for the data page, but `nextCursor` is `String(end)` and therefore
    // keeps a fractional end (e.g. limit 2.5 over a longer list yields nextCursor "2.5").
    let end = (start as f64 + limit).min(total as f64);
    let end_index = end as usize;
    Ok(json!({"data":models[start..end_index],"nextCursor":if end < total as f64 { Some(maho_ai::utils::js::number_to_string(end)) } else { None }}))
}

fn parse_model_list_cursor(cursor: Option<&Value>, total: usize) -> Result<usize, JsonRpcError> {
    let Some(value) = cursor.filter(|value| !value.is_null()) else {
        return Ok(0);
    };
    // Pinned validates `Number(cursor)` with a `/^\d+$/u` test; `RegExp.test` coerces the value to
    // its string form, so a numeric cursor whose string form is all digits is accepted too.
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let start = text
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| text.parse::<u64>().ok())
        .flatten()
        .filter(|start| *start <= 9_007_199_254_740_991);
    let Some(start) = start else {
        return Err(JsonRpcError::new(
            -32600,
            format!("model/list received an invalid cursor: {text}"),
        ));
    };
    let start = start as usize;
    if start > total {
        return Err(JsonRpcError::new(
            -32600,
            format!("model/list cursor {start} exceeds total models {total}"),
        ));
    }
    Ok(start)
}

fn normalize_model_list_limit(limit: Option<&Value>, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let Some(limit) = limit.filter(|value| !value.is_null()).and_then(Value::as_f64) else {
        return total as f64;
    };
    limit.max(1.0).min(total as f64)
}
