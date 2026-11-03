//! Port of senpi mcp/modern/inputs.ts: parse an `input_required` result's input requests.
use super::{
    errors::{McpMrtrError, McpMrtrErrorCode},
    form_schema::validate_mrtr_form,
};
use serde_json::{Map, Value};

fn byte_size(value: &Value) -> usize {
    serde_json::to_string(value).map_or(0, |text| text.len())
}
fn fail(code: McpMrtrErrorCode) -> McpMrtrError {
    McpMrtrError::new(code)
}

/// One `elicitation/create` input request (form mode only; URL mode is refused while parsing).
#[derive(Clone, Debug, PartialEq)]
pub struct McpMrtrInput {
    pub message: String,
    pub requested_schema: Value,
    pub params: Map<String, Value>,
}

fn check_strings(value: &Value) -> Result<(), McpMrtrError> {
    match value {
        Value::String(text) if text.len() > 16 * 1024 => Err(fail(McpMrtrErrorCode::InputLimit)),
        Value::Array(entries) => entries.iter().try_for_each(check_strings),
        Value::Object(entries) => entries.iter().try_for_each(|(key, entry)| {
            if key.len() > 16 * 1024 {
                return Err(fail(McpMrtrErrorCode::InputLimit));
            }
            check_strings(entry)
        }),
        _ => Ok(()),
    }
}

/// `Ok(None)` when the result carries only `requestState`; entries keep the server's key order.
pub fn parse_inputs(
    result: &Map<String, Value>,
) -> Result<Option<Vec<(String, McpMrtrInput)>>, McpMrtrError> {
    if !result.contains_key("requestState") && !result.contains_key("inputRequests") {
        return Err(fail(McpMrtrErrorCode::InvalidInput));
    }
    if result
        .get("requestState")
        .is_some_and(|state| !state.is_string())
    {
        return Err(fail(McpMrtrErrorCode::InvalidInput));
    }
    if byte_size(&Value::Object(result.clone())) > 512 * 1024
        || result
            .get("requestState")
            .and_then(Value::as_str)
            .is_some_and(|state| state.len() > 64 * 1024)
    {
        return Err(fail(McpMrtrErrorCode::InputLimit));
    }
    let Some(requests) = result.get("inputRequests") else {
        return Ok(None);
    };
    let Some(requests) = requests.as_object() else {
        return Err(fail(McpMrtrErrorCode::InvalidInput));
    };
    if requests.len() > 32 {
        return Err(fail(McpMrtrErrorCode::InputLimit));
    }
    // Scan the complete batch before collecting anything, including unsupported capability requests.
    for input in requests.values() {
        if input
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| ["roots/list", "sampling/createMessage"].contains(&method))
        {
            return Err(fail(McpMrtrErrorCode::UnsupportedInput));
        }
    }
    let mut inputs = Vec::new();
    for (key, input) in requests {
        check_strings(&Value::String(key.clone()))?;
        let params = input.get("params").and_then(Value::as_object);
        let (Some("elicitation/create"), Some(params)) =
            (input.get("method").and_then(Value::as_str), params)
        else {
            return Err(fail(McpMrtrErrorCode::InvalidInput));
        };
        let Some(message) = params.get("message").and_then(Value::as_str) else {
            return Err(fail(McpMrtrErrorCode::InvalidInput));
        };
        check_strings(&Value::Object(params.clone()))?;
        let mode = params.get("mode");
        if mode.and_then(Value::as_str) == Some("url") {
            if !params.get("url").is_some_and(Value::is_string)
                || !params.get("elicitationId").is_some_and(Value::is_string)
            {
                return Err(fail(McpMrtrErrorCode::InvalidInput));
            }
            return Err(fail(McpMrtrErrorCode::UnsupportedInput));
        }
        if mode.is_some_and(|mode| mode.as_str() != Some("form")) {
            return Err(fail(McpMrtrErrorCode::InvalidInput));
        }
        let schema = params
            .get("requestedSchema")
            .cloned()
            .unwrap_or(Value::Null);
        if schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.len() > 64)
        {
            return Err(fail(McpMrtrErrorCode::InputLimit));
        }
        if !validate_mrtr_form(&schema) {
            return Err(fail(McpMrtrErrorCode::InvalidInput));
        }
        inputs.push((
            key.clone(),
            McpMrtrInput {
                message: message.into(),
                requested_schema: schema,
                params: params.clone(),
            },
        ));
    }
    Ok(Some(inputs))
}
