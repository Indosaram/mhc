use std::future::Future;
use serde_json::{Value, json};
use super::handler::{CompletionError, CompletionRequest};

#[derive(Debug, PartialEq, Eq)]
pub struct CompletionToolCallSummary { pub ok: bool, pub error: Option<String> }

pub fn to_completion_request(value: &Value) -> Result<CompletionRequest, CompletionError> {
    let prompt = value.get("prompt").and_then(Value::as_str).ok_or_else(|| CompletionError("completion() received invalid arguments".into()))?;
    Ok(CompletionRequest { prompt: prompt.into(), opts:value.get("opts").cloned(), model:value["model"].as_str().map(str::to_owned), system:value["system"].as_str().map(str::to_owned), schema:value.get("schema").cloned() })
}

pub async fn handle_completion_tool_call<F, Fut>(call_id: &str, args: &Value, complete: F, is_active: impl Fn() -> bool, deliver: impl Fn(Value)) -> CompletionToolCallSummary
where F: FnOnce(CompletionRequest) -> Fut, Fut: Future<Output = Result<Value, CompletionError>> {
    let result = match to_completion_request(args) { Ok(request) => complete(request).await, Err(error) => Err(error) };
    match result {
        Ok(result) => {
            if !is_active() { return CompletionToolCallSummary { ok:false,error:Some("completion() result ignored after eval finalization".into()) }; }
            let value = result.get("value").or_else(|| result.get("text")).cloned().unwrap_or(Value::Null);
            deliver(json!({"type":"tool-reply","callId":call_id,"ok":true,"value":value}));
            CompletionToolCallSummary { ok:true,error:None }
        }
        Err(error) => {
            if is_active() { deliver(json!({"type":"tool-reply","callId":call_id,"ok":false,"error":{"message":error.0}})); }
            CompletionToolCallSummary { ok:false,error:Some(error.0) }
        }
    }
}
