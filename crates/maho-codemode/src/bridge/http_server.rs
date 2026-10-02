use std::{future::Future, pin::Pin, sync::Arc};
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Value, json};
use super::protocol::verify_bridge_token;

pub const DEFAULT_BODY_LIMIT_BYTES: usize = 1024 * 1024;
pub const LOOPBACK_HOST: &str = "127.0.0.1";

pub struct BridgeHttpCallRequest {
    pub call_id: String,
    pub tool_name: String,
    pub args: Value,
    pub signal: AbortSignal,
}

pub struct BridgeHttpCompletionRequest {
    pub prompt: String,
    pub opts: Option<Value>,
    pub signal: AbortSignal,
}

pub type BridgeHttpFuture<T> = Pin<Box<dyn Future<Output = Result<T, Value>> + Send>>;
pub type BridgeCallHandler = Arc<dyn Fn(BridgeHttpCallRequest) -> BridgeHttpFuture<Value> + Send + Sync>;
pub type BridgeCompletionHandler = Arc<dyn Fn(BridgeHttpCompletionRequest) -> BridgeHttpFuture<Value> + Send + Sync>;
pub type BridgeEmitHandler = Arc<dyn Fn(Value, AbortSignal) -> BridgeHttpFuture<()> + Send + Sync>;

pub struct BridgeServerOptions {
    pub token: Option<String>,
    pub body_limit_bytes: Option<usize>,
    pub on_call: BridgeCallHandler,
    pub on_emit: BridgeEmitHandler,
    pub on_completion: BridgeCompletionHandler,
}

fn transport_error(code: &str, message: &str) -> Value { json!({"ok":false,"error":{"code":code,"message":message}}) }

pub async fn dispatch_bridge_http_request(method: &str, url: &str, authorization: Option<&str>, token: &str, body: &[u8], options: &BridgeServerOptions, signal: AbortSignal) -> (u16, Option<Value>) {
    let route = url.split(['?', '#']).next().unwrap_or(url);
    if method != "POST" || !matches!(route,"/call"|"/emit"|"/completion") {
        return (404,Some(transport_error("not_found","Bridge route was not found")));
    }
    if authorization.and_then(|header|header.strip_prefix("Bearer ")).is_none_or(|auth|auth.is_empty() || verify_bridge_token(token,auth).is_err()) {
        return (401,Some(transport_error("unauthorized","Bridge authorization failed")));
    }
    let limit=options.body_limit_bytes.unwrap_or(DEFAULT_BODY_LIMIT_BYTES);
    if body.len()>limit {return (413,Some(transport_error("body_too_large",&format!("Bridge request body exceeds {limit} bytes"))));}
    let Ok(body)=serde_json::from_slice::<Value>(body) else {return (400,Some(transport_error("invalid_json","Bridge request body was not valid JSON")));};
    if route=="/emit" {
        let valid=match body["kind"].as_str() {
            Some("text")=>matches!(body["stream"].as_str(),Some("stdout"|"stderr")) && body["data"].is_string(),
            Some("display")=>body["mimeType"].is_string() && body["dataBase64"].is_string(),
            Some("log")=>body["message"].is_string(),
            Some("phase")=>body["title"].is_string(),
            _=>false,
        };
        if !valid {return (400,Some(transport_error("invalid_request","Bridge emit request was invalid")));}
        return match (options.on_emit)(body,signal).await {
            Ok(())=>(204,None),Err(error)=>(200,Some(json!({"ok":false,"error":error}))),
        };
    }
    let result=if route=="/call" {
        let (Some(call_id),Some(tool_name),Some(args))=(body["callId"].as_str(),body["toolName"].as_str(),body.get("args")) else {
            return (200,Some(transport_error("invalid_request","Bridge call request was invalid")));
        };
        (options.on_call)(BridgeHttpCallRequest{call_id:call_id.into(),tool_name:tool_name.into(),args:args.clone(),signal}).await
    } else {
        let Some(prompt)=body["prompt"].as_str() else {return (200,Some(transport_error("invalid_request","Bridge completion request was invalid")));};
        (options.on_completion)(BridgeHttpCompletionRequest{prompt:prompt.into(),opts:body.get("opts").cloned(),signal}).await
    };
    (200,Some(match result {Ok(value)=>json!({"ok":true,"value":value}),Err(error)=>json!({"ok":false,"error":error})}))
}
