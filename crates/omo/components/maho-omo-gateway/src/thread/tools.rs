//! The thread tools: what a model calls to reach another session or a bound conversation.
//!
//! Upstream counterpart: `thread/tools.ts`. Every failure is data (`{ kind: "error", error }`), and a
//! session op's tool is its own namespace (`ext_<extension>_<toolName>`).

use std::sync::Arc;

use serde_json::{Value, json};

use super::adapter::BoxFuture;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::thread_tool_failure;
use super::relay::{AnswerRequest, GatewayRelay, Keyed};
use super::store_api::GatewayStoreApi;
use super::store_relay_ops::{BindInput, BindingsFilter, ReportOpRequest};
use super::store_extensions::{DeclaredSessionOp, SessionCaller, StoreExtensionResult};
use super::types::{GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender};

#[derive(Clone)]
pub struct ThreadTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub execute: Arc<dyn Fn(&str, Value) -> BoxFuture<Value> + Send + Sync>,
}

impl std::fmt::Debug for ThreadTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ThreadTool").field("name", &self.name).finish()
    }
}

pub struct ThreadToolsOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub relay: GatewayRelay,
    /// The calling session's durable id, resolved by the engine, never taken from a tool argument.
    pub caller_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub caller_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    /// The caller's own provenance, resolved in its runtime.
    pub caller_provenance: Arc<dyn Fn() -> super::types::Provenance + Send + Sync>,
}

fn error_result(error: super::errors::ThreadToolFailure) -> Value {
    json!({ "kind": "error", "error": error })
}

fn ok_result(value: Value) -> Value {
    let mut result = value;
    if let Some(object) = result.as_object_mut() {
        object.insert("kind".into(), Value::String("ok".into()));
    }
    result
}

fn invalid_arguments(message: impl Into<String>) -> Value {
    error_result(thread_tool_failure("invalid_arguments", message, "Fix the arguments and call again.", None))
}

fn delivery_result(result: GatewayDeliveryResult, link: String) -> Value {
    match result {
        GatewayDeliveryResult::Ok { thread_id, delivery_id, message_seq, delivery, effective_mode, endpoint_kind, deduplicated } => ok_result(json!({
            "thread_id": thread_id,
            "link": link,
            "delivery": delivery,
            "message_seq": message_seq,
            "deduplicated": deduplicated,
            "delivery_id": delivery_id,
            "effective_mode": effective_mode,
            "endpoint": endpoint_kind.map(|kind| kind.as_str()),
        })),
        GatewayDeliveryResult::Error { error } => error_result(error),
    }
}

fn thread_link(thread_id: &str, title: &str) -> String {
    super::contracts_results::thread_link(thread_id, title)
}

/// Every thread tool this component owns. `thread_list` and `thread_read` read the address book and a
/// transcript; they are registered by the component that owns those modules.
pub fn create_thread_tools(options: Arc<ThreadToolsOptions>) -> Vec<ThreadTool> {
    let mut tools = Vec::new();

    for (name, description) in [
        ("thread_send", "Send a message to another thread and report how it was delivered."),
        ("thread_handoff", "Hand work to another thread, resolving its address first."),
    ] {
        let options = options.clone();
        let tool_name = name.to_owned();
        tools.push(ThreadTool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "minLength": 1 },
                    "message": { "type": "string", "minLength": 1 },
                    "delivery": { "type": "string", "enum": ["auto", "steer", "follow_up"] },
                    "expected_turn_id": { "type": "integer" },
                },
                "required": ["thread", "message"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |call_id: &str, args: Value| {
                let options = options.clone();
                let tool_name = tool_name.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let Some(thread) = args.get("thread").and_then(Value::as_str) else {
                        return invalid_arguments("thread must be a non-empty string");
                    };
                    let Some(message) = args.get("message").and_then(Value::as_str) else {
                        return invalid_arguments("message must be a non-empty string");
                    };
                    let mode = match args.get("delivery").and_then(Value::as_str) {
                        Some(mode) => match GatewayDeliveryMode::parse(mode) {
                            Some(mode) => Some(mode),
                            None => return invalid_arguments("delivery must be auto, steer or follow_up"),
                        },
                        None => None,
                    };
                    let sender = GatewaySender::Session {
                        durable_id: (options.caller_session_id)(),
                        name: options.caller_name.as_ref().and_then(|name| name()),
                        turn_id: None,
                        cause_delivery_id: None,
                    };
                    // Forged provenance arguments never reach the envelope: the sender runtime's own
                    // context is the only source.
                    let provenance = (options.caller_provenance)();
                    let provenance = Arc::new(provenance);
                    let engine = GatewayEngine::new(super::engine::GatewayEngineOptions {
                        sender_provenance: Some(Arc::new(move || (*provenance).clone())),
                        store: options.store.clone(),
                        endpoints: options.engine.options().endpoints.clone(),
                        resolve: options.engine.options().resolve.clone(),
                        now: options.engine.options().now.clone(),
                    });
                    let mut request = GatewayDeliverRequest::new(sender, thread, message);
                    request.mode = mode;
                    request.expected_turn_id = args.get("expected_turn_id").and_then(Value::as_i64);
                    request.idempotency_key = Some(call_id.clone());
                    let result = engine.deliver(request).await;
                    let _ = &tool_name;
                    delivery_result(result, thread_link(thread, thread))
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_report".into(),
            description: "Report an event to the conversation bound to this session.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "event": { "type": "string", "enum": ["milestone", "report", "question", "completion"] },
                    "text": { "type": "string", "minLength": 1 },
                    "binding_id": { "type": "string" },
                    "request_id": { "type": "string" },
                    "request_kind": { "type": "string", "enum": ["question", "select", "confirm", "input", "editor"] },
                },
                "required": ["event", "text"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |call_id: &str, args: Value| {
                let options = options.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let (Some(event), Some(text)) = (args.get("event").and_then(Value::as_str), args.get("text").and_then(Value::as_str)) else {
                        return invalid_arguments("event and text are required");
                    };
                    let request = ReportOpRequest {
                        session_durable_id: (options.caller_session_id)(),
                        binding_id: args.get("binding_id").and_then(Value::as_str).map(str::to_owned),
                        origin_delivery_ids: Vec::new(),
                        origin_local_input: false,
                        event: event.to_owned(),
                        text: text.to_owned(),
                        ui_request_id: args.get("request_id").and_then(Value::as_str).map(str::to_owned),
                        ui_request_kind: args.get("request_kind").and_then(Value::as_str).map(str::to_owned),
                    };
                    let _ = call_id;
                    match options.relay.report(&request) {
                        Ok(report) => ok_result(json!({
                            "binding_id": report.binding_id,
                            "revision": report.revision,
                            "event": report.event,
                            "cursor": report.cursor,
                            "reply_token": report.reply_token,
                            "armed": report.armed,
                            "deduplicated": false,
                        })),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_bind".into(),
            description: "Bind this session to a conversation thread.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "platform": { "type": "string" },
                    "account_id": { "type": "string" },
                    "chat_id": { "type": "string" },
                    "thread_id": { "type": "string" },
                    "ttl_seconds": { "type": "integer" },
                },
                "required": ["platform", "account_id", "chat_id"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |_call_id: &str, args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let mut input: BindInput = match serde_json::from_value(json!({
                        "platform": args.get("platform").cloned().unwrap_or(Value::Null),
                        "account_id": args.get("account_id").cloned().unwrap_or(Value::Null),
                        "chat_id": args.get("chat_id").cloned().unwrap_or(Value::Null),
                        "thread_id": args.get("thread_id").cloned(),
                        "session_durable_id": (options.caller_session_id)(),
                        "ttl_seconds": args.get("ttl_seconds").cloned(),
                    })) {
                        Ok(input) => input,
                        Err(error) => return invalid_arguments(error.to_string()),
                    };
                    input.ttl_seconds = args.get("ttl_seconds").and_then(Value::as_i64);
                    match options.relay.bind(&input) {
                        Ok(binding) => ok_result(json!({ "binding": binding, "deduplicated": false })),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_bindings".into(),
            description: "List this session's conversation bindings.".into(),
            parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            execute: Arc::new(move |_call_id: &str, _args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let filter = BindingsFilter { session_durable_id: Some((options.caller_session_id)()), ..Default::default() };
                    match options.relay.bindings(&filter, None, None) {
                        Ok((bindings, next_cursor)) => ok_result(json!({ "bindings": bindings, "next_cursor": next_cursor })),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_outbox".into(),
            description: "Read the rows this session reported to a binding.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "binding_id": { "type": "string" }, "after_cursor": { "type": "integer" }, "limit": { "type": "integer" } },
                "required": ["binding_id"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |_call_id: &str, args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let Some(binding_id) = args.get("binding_id").and_then(Value::as_str) else {
                        return invalid_arguments("binding_id is required");
                    };
                    match options.relay.outbox(binding_id, args.get("after_cursor").and_then(Value::as_i64), args.get("limit").and_then(Value::as_i64)) {
                        Ok(page) => ok_result(json!(page)),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_answer".into(),
            description: "Answer a question this session asked through a binding.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "binding_id": { "type": "string" }, "reply_token": { "type": "string" }, "answer": { "type": "string" } },
                "required": ["binding_id", "reply_token", "answer"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |_call_id: &str, args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let (Some(binding_id), Some(reply_token), Some(answer)) = (
                        args.get("binding_id").and_then(Value::as_str),
                        args.get("reply_token").and_then(Value::as_str),
                        args.get("answer").and_then(Value::as_str),
                    ) else {
                        return invalid_arguments("binding_id, reply_token and answer are required");
                    };
                    let request = AnswerRequest { binding_id: binding_id.to_owned(), reply_token: reply_token.to_owned(), answer: answer.to_owned(), author: None };
                    match options.relay.answer(&request).await {
                        Ok((claim, answered_by)) => ok_result(json!({
                            "binding_id": binding_id,
                            "cursor": claim.cursor,
                            "session_durable_id": claim.session_durable_id,
                            "answered_by": answered_by,
                        })),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    tools
}

/// The tools a session's registered extension ops add: `ext_<extension>_<toolName>`, each waiting on
/// its declared await.
pub fn register_extension_tools(store: Arc<dyn GatewayStoreApi>, caller: Arc<dyn Fn() -> String + Send + Sync>) -> Vec<ThreadTool> {
    store
        .session_callable_ops()
        .into_iter()
        .map(|declared| extension_tool(store.clone(), caller.clone(), declared))
        .collect()
}

fn extension_tool(store: Arc<dyn GatewayStoreApi>, caller: Arc<dyn Fn() -> String + Send + Sync>, declared: DeclaredSessionOp) -> ThreadTool {
    let name = declared.registered_name.clone();
    let description = declared.entry.description.clone().unwrap_or_default();
    let parameters = declared.entry.parameters.clone();
    let extension = declared.extension.clone();
    let op = declared.entry.op.clone();
    ThreadTool {
        name,
        description,
        parameters,
        execute: Arc::new(move |_call_id: &str, args: Value| {
            let store = store.clone();
            let caller = caller.clone();
            let extension = extension.clone();
            let op = op.clone();
            Box::pin(async move {
                let session_caller = SessionCaller { caller_durable_id: caller() };
                match store.extension_session_await(&extension, &op, &args, &session_caller, store.now()) {
                    StoreExtensionResult::Ok { value } => ok_result(json!({ "value": value })),
                    StoreExtensionResult::Refused { code, message } => error_result(thread_tool_failure(
                        if code.as_str() == "invalid_arguments" { "invalid_arguments" } else { "unsupported" },
                        message,
                        "Read the extension's declared parameters and retry.",
                        Some(json!({ "extension_refusal": code.as_str() })),
                    )),
                }
            })
        }),
    }
}

/// The key a tool call's receipt is recorded under.
pub fn call_key(principal: &str, call_id: &str) -> Keyed {
    Keyed { principal: principal.to_owned(), idempotency_key: Some(call_id.to_owned()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_key_keeps_the_idempotency_key() {
        let keyed = call_key("session:sender", "call-1");
        assert_eq!(keyed.idempotency_key.as_deref(), Some("call-1"));
    }

    #[test]
    fn an_extension_tool_is_named_in_its_own_namespace() {
        let declared = DeclaredSessionOp {
            entry: super::super::store_extensions::SessionCallableOp {
                op: "open".into(),
                parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                target_arg: None,
                internal: None,
                tool_name: Some("open".into()),
                description: Some("Open.".into()),
                await_spec: None,
                wake: None,
            },
            extension: "alpha".into(),
            registered_name: "ext_alpha_open".into(),
        };
        assert_eq!(super::super::extension_registrations::registered_tool_name(&declared.extension, "open"), declared.registered_name);
    }
}
