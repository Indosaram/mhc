use super::types::{EvalLanguage, EvalToolInput, EvalToolRequest, TimeoutBehavior};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct EvalRequestError(pub String);

pub const fn isolate_invalid_code() -> &'static str { "eval_isolate_invalid" }

impl EvalRequestError {
    pub fn code(&self) -> Option<&'static str> {
        self.0.starts_with("eval_isolate_invalid:").then_some(isolate_invalid_code())
    }
}

pub fn normalize_eval_summary(value: &Value) -> Option<String> {
    let value = value.as_str()?;
    let normalized = value.split(|c: char| c.is_whitespace() || c == '\u{feff}').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

pub fn parse_eval_request(params: &Value) -> Result<EvalToolRequest, EvalRequestError> {
    parse_eval_request_with_sandbox(params, false)
}

pub fn parse_eval_request_with_sandbox(params: &Value, sandbox: bool) -> Result<EvalToolRequest, EvalRequestError> {
    let params = params.as_object().ok_or_else(|| EvalRequestError("eval parameters must be an object".into()))?;
    let action = params.get("action").and_then(Value::as_str);
    if action == Some("list") { return Ok(EvalToolRequest::List); }
    if matches!(action, Some("peek" | "stop")) {
        let cell_id = params.get("cell_id").and_then(Value::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| EvalRequestError(format!("eval action \"{}\" requires cell_id", action.unwrap_or(""))))?.to_owned();
        return Ok(if action == Some("peek") { EvalToolRequest::Peek { cell_id } } else { EvalToolRequest::Stop { cell_id } });
    }
    if let Some(value) = params.get("action") && action != Some("run") {
        return Err(EvalRequestError(format!("Unknown eval action \"{}\"", js_string(value))));
    }
    let language = match params.get("language").and_then(Value::as_str) {
        Some("js") => EvalLanguage::Js, Some("py") => EvalLanguage::Py,
        Some("rb") => EvalLanguage::Rb, Some("jl") => EvalLanguage::Jl,
        _ => return Err(EvalRequestError("eval run requires language".into())),
    };
    let code = params.get("code").and_then(Value::as_str).ok_or_else(|| EvalRequestError("eval run requires code".into()))?.to_owned();
    let summary = params.get("summary").and_then(normalize_eval_summary).ok_or_else(|| EvalRequestError("eval run requires summary — one line in the user's language: what you are working on and for what purpose".into()))?;
    let on_timeout = match params.get("on_timeout") {
        None => None,
        Some(Value::String(value)) if value == "detach" => Some(TimeoutBehavior::Detach),
        Some(Value::String(value)) if value == "error" => Some(TimeoutBehavior::Error),
        Some(value) => return Err(EvalRequestError(format!("Unknown eval on_timeout value \"{}\"", js_string(value)))),
    };
    let isolate = match params.get("isolate") {
        None | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) if !sandbox => return Err(EvalRequestError("eval_isolate_invalid: sandbox cells are turned off (set sandbox.enabled in codemode settings)".into())),
        Some(Value::Bool(true)) if language != EvalLanguage::Js => return Err(EvalRequestError("eval_isolate_invalid: sandbox cells run JavaScript only (language: \"js\")".into())),
        Some(Value::Bool(true)) if params.get("reset") == Some(&Value::Bool(true)) => return Err(EvalRequestError("eval_isolate_invalid: a sandbox cell starts fresh; reset does not apply".into())),
        Some(Value::Bool(true)) => Some(true),
        Some(_) => return Err(EvalRequestError("eval_isolate_invalid: isolate must be true or false".into())),
    };
    Ok(EvalToolRequest::Run(EvalToolInput { language, code, summary, action: action.map(String::from), timeout: params.get("timeout").and_then(Value::as_f64), on_timeout, reset: params.get("reset").and_then(Value::as_bool), isolate }))
}

fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(values) => values.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(","),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

pub fn is_eval_control_request(request: &EvalToolRequest) -> bool {
    matches!(request, EvalToolRequest::List | EvalToolRequest::Peek { .. } | EvalToolRequest::Stop { .. })
}

pub fn eval_timeout_behavior(input: &EvalToolInput, mode: &str) -> TimeoutBehavior {
    input.on_timeout.unwrap_or(if matches!(mode, "print" | "json") { TimeoutBehavior::Error } else { TimeoutBehavior::Detach })
}

#[cfg(test)]
mod isolate_tests {
    use super::*;

    fn request(language: &str, extras: Value) -> Value {
        let mut value = serde_json::json!({"language":language,"code":"1+1","summary":"test"});
        value.as_object_mut().unwrap().extend(extras.as_object().unwrap().clone());
        value
    }

    #[test]
    fn isolate_requires_enabled_js_sandbox_and_fresh_state() {
        assert_eq!(match parse_eval_request_with_sandbox(&request("js", serde_json::json!({"isolate":true})), true).unwrap() { EvalToolRequest::Run(input) => input.isolate, _ => None }, Some(true));
        assert_eq!(parse_eval_request_with_sandbox(&request("js", serde_json::json!({"isolate":true})), false).unwrap_err().code(), Some("eval_isolate_invalid"));
        assert!(parse_eval_request_with_sandbox(&request("py", serde_json::json!({"isolate":true})), true).unwrap_err().to_string().contains("JavaScript only"));
        assert!(parse_eval_request_with_sandbox(&request("js", serde_json::json!({"isolate":true,"reset":true})), true).unwrap_err().to_string().contains("reset does not apply"));
        assert!(parse_eval_request_with_sandbox(&request("js", serde_json::json!({"isolate":"yes"})), true).unwrap_err().to_string().contains("isolate must be true or false"));
    }
}
