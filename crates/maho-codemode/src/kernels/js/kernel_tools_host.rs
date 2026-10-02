use std::{collections::HashMap, sync::{Arc, Mutex}};
use serde_json::{Value, json};
use tokio::sync::{oneshot, broadcast};
use super::{kernel_tools_types::*, kernel_tools_errors::{KernelToolError, kernel_tool_error}};

type Waiter = oneshot::Sender<Result<Value, KernelToolError>>;
struct RequestGuard<'a> {
    pump: &'a KernelToolHostPump,
    request_id: String,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        let removed = self.pump.waiters.lock().expect("kernel tool waiters poisoned").remove(&self.request_id).is_some();
        if removed { (self.pump.post)(json!({"type":"kernel-tool-cancel","requestId":self.request_id})); }
    }
}

pub struct KernelToolHostPump {
    waiters: Mutex<HashMap<String, Waiter>>,
    post: Arc<dyn Fn(Value) + Send + Sync>,
    is_open: Arc<dyn Fn() -> bool + Send + Sync>,
    events: broadcast::Sender<&'static str>,
}

impl KernelToolHostPump {
    pub fn new(post: Arc<dyn Fn(Value) + Send + Sync>, is_open: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self { waiters:Mutex::new(HashMap::new()), post, is_open, events }
    }
    pub fn events(&self) -> broadcast::Receiver<&'static str> { self.events.subscribe() }

    pub fn consume(&self, message: Value) -> bool {
        if message["type"] == "tool-call" && message["toolName"] == crate::bridge::reserved::RESERVED_AGENT_TOOL { let _ = self.events.send("outerAwaitingAgent"); }
        if !matches!(message["type"].as_str(), Some("kernel-tool-describe-reply" | "kernel-tool-invoke-reply")) { return false; }
        if let Some(request_id) = message["requestId"].as_str() && let Some(waiter) = self.waiters.lock().expect("kernel tool waiters poisoned").remove(request_id) { let _ = waiter.send(Ok(message)); }
        true
    }

    pub fn reject_all(&self, error: KernelToolError) {
        let waiters = std::mem::take(&mut *self.waiters.lock().expect("kernel tool waiters poisoned"));
        for waiter in waiters.into_values() { let _ = waiter.send(Err(error.clone())); }
    }

    async fn request(&self, message: Value, signal: Option<maho_ext_api::AbortSignal>) -> Result<Value, KernelToolError> {
        if !(self.is_open)() { return Err(kernel_tool_error(KernelToolErrorCode::ToolsUnavailable, "JavaScript worker is not available", None)); }
        let request_id = message["requestId"].as_str().expect("host request id").to_owned();
        let (sender, mut receiver) = oneshot::channel();
        self.waiters.lock().expect("kernel tool waiters poisoned").insert(request_id.clone(), sender);
        let _guard = RequestGuard { pump: self, request_id: request_id.clone() };
        if !signal.as_ref().is_some_and(|signal|signal.is_aborted()) { (self.post)(message); }
        if let Some(signal) = signal {
            tokio::select! {
                biased;
                result = &mut receiver => result.map_err(|_|kernel_tool_error(KernelToolErrorCode::ToolsUnavailable, "JavaScript worker is not available", None))?,
                () = signal.cancelled() => {
                    self.waiters.lock().expect("kernel tool waiters poisoned").remove(&request_id);
                    (self.post)(json!({"type":"kernel-tool-cancel","requestId":request_id}));
                    Err(kernel_tool_error(KernelToolErrorCode::KernelToolStale, "Kernel tool call cancelled", None))
                }
            }
        } else { receiver.await.map_err(|_|kernel_tool_error(KernelToolErrorCode::ToolsUnavailable, "JavaScript worker is not available", None))? }
    }

    pub async fn describe(&self, names: &[String]) -> Result<Value, KernelToolError> {
        let reply = self.request(json!({"type":"kernel-tool-describe","requestId":crate::bridge::protocol::generate_correlation_id(),"names":names}), None).await?;
        if reply["type"] != "kernel-tool-describe-reply" { return Err(kernel_tool_error(KernelToolErrorCode::KernelToolFailed, "unexpected kernel-tool describe reply", None)); }
        if reply["ok"] != true { return Err(reply_error(&reply)); }
        Ok(json!({"results":reply["results"]}))
    }

    pub async fn invoke(&self, request: KernelToolsInvokeRequest, options: KernelToolsInvokeOptions) -> Result<Value, KernelToolError> {
        let _ = self.events.send("nestedInvoke");
        let mut message = json!({"type":"kernel-tool-invoke","requestId":crate::bridge::protocol::generate_correlation_id(),"name":request.name,"kernel_generation":request.kernel_generation,"definition_revision":request.definition_revision,"args":request.args,"call_id":request.call_id});
        if let Some(scope) = options.scope && (scope.allow.is_some() || scope.deny.is_some()) {
            let mut tools = serde_json::Map::new();
            if let Some(allow) = scope.allow { tools.insert("allow".into(), json!(allow)); }
            if let Some(deny) = scope.deny { tools.insert("deny".into(), json!(deny)); }
            message["scope"] = json!({"tools":tools});
        }
        let reply = self.request(message, options.signal).await?;
        if reply["type"] != "kernel-tool-invoke-reply" { return Err(kernel_tool_error(KernelToolErrorCode::KernelToolFailed, "unexpected kernel-tool invoke reply", None)); }
        if reply["ok"] != true { return Err(reply_error(&reply)); }
        Ok(reply["value"].clone())
    }
}

fn reply_error(reply: &Value) -> KernelToolError {
    let code = match reply["error"]["code"].as_str() {
        Some("kernel_tool_stale")=>KernelToolErrorCode::KernelToolStale,
        Some("kernel_tool_missing")=>KernelToolErrorCode::KernelToolMissing,
        Some("kernel_tool_recursion")=>KernelToolErrorCode::KernelToolRecursion,
        Some("kernel_tool_host_denied")=>KernelToolErrorCode::KernelToolHostDenied,
        Some("tools_unavailable")=>KernelToolErrorCode::ToolsUnavailable,
        Some("invalid_tool_definition")=>KernelToolErrorCode::InvalidToolDefinition,
        _=>KernelToolErrorCode::KernelToolFailed,
    };
    kernel_tool_error(code, reply["error"]["message"].as_str().unwrap_or(""), reply["error"].get("details").and_then(|details|serde_json::from_value(details.clone()).ok()))
}
