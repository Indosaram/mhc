use std::sync::{Arc, Mutex};
use maho_ext_api::{
    AgentToolResult, ContentBlock, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent, InputSource, JsonValue, ToolDefinition, ToolExecutionMode,
    ToolExposure,
};
use serde_json::json;

pub const BROWSER_BRIDGE_TOOL_NAME: &str = "omo_browser_bridge";
pub const BROWSER_STATE_EVENT: &str = "omo.browser.state";
pub const BROWSER_STOP_RPC: &str = "omo.browser.stop";

pub fn parameters_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "op": { "type": "string", "enum": ["state", "status", "stopped"] },
            "data": { "type": "object", "additionalProperties": true }
        },
        "required": ["op"],
        "additionalProperties": false
    })
}

fn is_state_payload(value: Option<&JsonValue>) -> bool {
    let Some(value) = value else { return false; };
    let Some(map) = value.as_object() else { return false; };
    matches!(
        (map.get("engine"), map.get("status")),
        (Some(JsonValue::String(_)), Some(JsonValue::String(_)))
    )
}

fn reply_agent_result(details: JsonValue) -> AgentToolResult {
    let text = serde_json::to_string(&details).unwrap_or_default();
    AgentToolResult {
        content: vec![ContentBlock::text(text)],
        details,
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

#[derive(Clone, Default)]
pub struct BrowserBridgeComponent {
    stopped: Arc<Mutex<bool>>,
}

impl BrowserBridgeComponent {
    pub fn new() -> Self {
        Self {
            stopped: Arc::new(Mutex::new(false)),
        }
    }

    pub fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_stopped(&self, value: bool) {
        *self.stopped.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = value;
    }
}

pub fn create_browser_bridge_component() -> BrowserBridgeComponent {
    BrowserBridgeComponent::new()
}

impl Extension for BrowserBridgeComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let stopped = self.stopped.clone();
        let stopped_for_tool = stopped.clone();
        let stopped_for_rpc = stopped.clone();
        let stopped_for_input = stopped.clone();

        let mut definition = ToolDefinition::new(
            BROWSER_BRIDGE_TOOL_NAME,
            "Internal channel for the browser skill: publish browser state to the app and read whether the user stopped browser use. Not for direct use.",
            parameters_schema(),
            Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
        );
        definition.label = "Browser bridge".to_string();
        definition.exposure = Some(ToolExposure::Eval);
        definition.execution_mode = Some(ToolExecutionMode::Parallel);
        definition.permission_parser = Some(Arc::new(|_input, _cwd| Vec::new()));

        let events = api.events.clone();
        let execute: maho_ext_api::ExtensionToolExecutor = Arc::new(move |_id: &str, params: JsonValue, _signal, _update, _context: &maho_ext_api::ExtensionContext| {
            let stopped = stopped_for_tool.clone();
            let events = events.clone();
            Box::pin(async move {
                let op = params.get("op").and_then(JsonValue::as_str);
                let current_stopped = *stopped.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

                if op == Some("state") {
                    let data = params.get("data");
                    if !is_state_payload(data) {
                        return Ok(reply_agent_result(json!({
                            "ok": false,
                            "stopped": current_stopped,
                            "reason": "invalid_state"
                        })));
                    }
                    if let Some(data) = data {
                        let event = json!({
                            "name": BROWSER_STATE_EVENT,
                            "data": data.clone()
                        });
                        events.emit("senpi:extension-rpc-event", &event);
                    }
                    return Ok(reply_agent_result(json!({
                        "ok": true,
                        "stopped": current_stopped
                    })));
                }

                if op == Some("stopped") {
                    *stopped.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                    return Ok(reply_agent_result(json!({
                        "ok": true,
                        "stopped": true
                    })));
                }

                Ok(reply_agent_result(json!({
                    "ok": true,
                    "stopped": current_stopped
                })))
            })
        });

        if let Err(error) = api.register_tool_with_extension_context(definition, execute) {
            std::panic::panic_any(error);
        }

        if let Err(error) = api.rpc_handle(
            BROWSER_STOP_RPC,
            Arc::new(move |_params| {
                let stopped = stopped_for_rpc.clone();
                Box::pin(async move {
                    *stopped.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                    Ok(json!({ "stopped": true }))
                })
            }),
        ) {
            std::panic::panic_any(error);
        }

        api.on(
            EventKind::Input,
            Arc::new(move |event, _ctx| {
                let stopped = stopped_for_input.clone();
                Box::pin(async move {
                    if let ExtensionEvent::Input(input_event) = event
                        && input_event.source != InputSource::Extension {
                        *stopped.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
