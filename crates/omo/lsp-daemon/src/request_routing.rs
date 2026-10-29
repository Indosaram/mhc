//! Port of `request-routing.ts`: authenticate, strip `_context`, and dispatch to lsp-core.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use lsp_core::abort::{AbortController, AbortSignal};
use lsp_core::mcp::{HandleLspMcpRequestOptions, handle_lsp_mcp_request};
use lsp_core::request_context::{
    LspRequestContext, parse_lsp_request_context, scope_request_context,
};
use serde_json::{Map, Value, json};

use crate::ipc_protocol::{OMO_DAEMON_PROTOCOL_VERSION, authenticate_message};
use crate::ownership::DaemonOwner;

pub const CONTEXT_KEY: &str = "_context";

/// Result of `extract_request_context` (TS `RoutedRequest`).
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedRequest {
    pub input: Value,
    pub context: Option<LspRequestContext>,
}

/// TS `InvalidDaemonRequestError`: the message is surfaced verbatim to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDaemonRequestError(pub String);

impl std::fmt::Display for InvalidDaemonRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidDaemonRequestError {}

fn invalid(message: &str) -> InvalidDaemonRequestError {
    InvalidDaemonRequestError(message.to_string())
}

/// TS `extractRequestContext`.
pub fn extract_request_context(raw: &Value) -> Result<RoutedRequest, InvalidDaemonRequestError> {
    let Some(record) = raw
        .as_object()
        .filter(|record| record.get("method") == Some(&json!("tools/call")))
    else {
        return Ok(RoutedRequest {
            input: raw.clone(),
            context: None,
        });
    };
    let params = record
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("Daemon tools/call params must be an object."))?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("Daemon tools/call arguments must be an object."))?;
    let context_value = args
        .get(CONTEXT_KEY)
        .ok_or_else(|| invalid("Daemon tools/call arguments must include _context."))?;
    if !context_value.is_object() {
        return Err(invalid("LSP request _context must be an object."));
    }
    let context = parse_lsp_request_context(context_value)
        .map_err(|error| InvalidDaemonRequestError(error.message))?;
    let mut cleaned_args = args.clone();
    cleaned_args.remove(CONTEXT_KEY);
    let mut cleaned_params = params.clone();
    cleaned_params.insert("arguments".to_string(), Value::Object(cleaned_args));
    let mut cleaned = record.clone();
    cleaned.insert("params".to_string(), Value::Object(cleaned_params));
    Ok(RoutedRequest {
        input: Value::Object(cleaned),
        context: Some(context),
    })
}

pub type DispatchFuture = Pin<Box<dyn Future<Output = Option<Value>> + Send>>;

/// Core dispatcher seam (TS `handleLspMcpRequest`, mocked by the routing tests).
pub type Dispatch = Arc<
    dyn Fn(Value, Option<LspRequestContext>, Option<AbortSignal>) -> DispatchFuture + Send + Sync,
>;

/// Real dispatcher: lsp-core's MCP handler scoped to the request context.
pub fn core_dispatch() -> Dispatch {
    Arc::new(|input, context, signal| {
        Box::pin(async move {
            let options = HandleLspMcpRequestOptions { signal };
            let response = match context {
                Some(context) => {
                    scope_request_context(context, handle_lsp_mcp_request(&input, options)).await
                }
                None => handle_lsp_mcp_request(&input, options).await,
            };
            response.and_then(|response| serde_json::to_value(response).ok())
        })
    })
}

/// One in-flight request; `serial` distinguishes reuses of the same JSON-RPC id.
#[derive(Debug, Clone)]
pub struct ActiveRequest {
    pub serial: u64,
    pub controller: AbortController,
}

pub type ActiveRequests = Arc<Mutex<HashMap<String, ActiveRequest>>>;

static NEXT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// TS `DaemonRouteState`.
#[derive(Clone)]
pub struct DaemonRouteState {
    pub token: String,
    pub owner: DaemonOwner,
    pub active_requests: Option<ActiveRequests>,
    pub dispatch: Dispatch,
}

fn lock_active(
    active: &ActiveRequests,
) -> std::sync::MutexGuard<'_, HashMap<String, ActiveRequest>> {
    active.lock().unwrap_or_else(PoisonError::into_inner)
}

/// TS `handleDaemonMessage`: `None` means no reply (notifications, cancellation).
pub async fn handle_daemon_message(raw: &Value, state: &DaemonRouteState) -> Option<Value> {
    let authenticated = match authenticate_message(raw, &state.token) {
        Ok(authenticated) => authenticated,
        Err(error) => return Some(error),
    };
    match authenticated.method.as_deref() {
        Some("omo/ping") => {
            let mut result = Map::new();
            result.insert(
                "protocolVersion".to_string(),
                json!(OMO_DAEMON_PROTOCOL_VERSION),
            );
            if let Value::Object(owner) = state.owner.to_json() {
                result.extend(owner);
            }
            return Some(json!({"jsonrpc": "2.0", "id": authenticated.id, "result": result}));
        }
        Some("$/cancelRequest") => {
            if let (Some(target), Some(active)) = (
                cancellation_target_id(&authenticated.input),
                &state.active_requests,
            ) {
                let controller = lock_active(active)
                    .get(&target)
                    .map(|entry| entry.controller.clone());
                if let Some(controller) = controller {
                    controller.abort();
                }
            }
            return None;
        }
        _ => {}
    }
    let routed = match extract_request_context(&Value::Object(authenticated.input)) {
        Ok(routed) => routed,
        Err(error) => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": authenticated.id,
                "error": {"code": -32602, "message": error.0, "data": {"code": "invalid_daemon_request"}}
            }));
        }
    };
    let (Some(key), Some(active)) = (route_request_key(&authenticated.id), &state.active_requests)
    else {
        return (state.dispatch)(routed.input, routed.context, None).await;
    };
    let controller = AbortController::new();
    let serial = NEXT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    lock_active(active).insert(
        key.clone(),
        ActiveRequest {
            serial,
            controller: controller.clone(),
        },
    );
    let response = (state.dispatch)(routed.input, routed.context, Some(controller.signal())).await;
    let mut active = lock_active(active);
    if active
        .get(&key)
        .is_some_and(|current| current.serial == serial)
    {
        active.remove(&key);
    }
    response
}

fn route_request_key(id: &Value) -> Option<String> {
    match id {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn cancellation_target_id(input: &Map<String, Value>) -> Option<String> {
    route_request_key(input.get("params")?.as_object()?.get("id")?)
}

#[cfg(test)]
#[path = "request_routing_tests.rs"]
mod tests;
