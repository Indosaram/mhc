use maho_ext_api::{BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use serde_json::{Value, json};
use std::sync::Arc;

pub const ANTHROPIC_BASH_SECTION: &str = "\n## Bash Tool\n\nThe native bash tool is available in this session. The model has direct\nshell access via the bash_20250124 tool. The session is stateless — each\ncommand runs independently. The 'restart' parameter is accepted but has\nno effect (no persistent shell session). Standard senpi safety\nguardrails still apply.\n";

pub fn parse_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

pub fn is_anthropic_bash_enabled() -> bool {
    parse_enabled(std::env::var("PI_ANTHROPIC_BASH").ok().as_deref())
}

fn native(tool: &Value) -> bool {
    tool.get("type").and_then(Value::as_str).is_some_and(|kind| kind.starts_with("bash_"))
}

pub fn add_anthropic_bash_to_payload(api: Option<&str>, payload: &Value, enabled: bool) -> Value {
    if api != Some("anthropic-messages") || !enabled || !payload.is_object() { return payload.clone(); }
    let mut tools: Vec<Value> = payload.get("tools").and_then(Value::as_array).into_iter().flatten()
        .filter(|tool| tool.is_object() && (tool.get("name").and_then(Value::as_str) != Some("bash") || native(tool)))
        .cloned().collect();
    if !tools.iter().any(native) { tools.push(json!({"type":"bash_20250124","name":"bash"})); }
    let mut result = payload.clone();
    result["tools"] = Value::Array(tools);
    result
}

pub struct AnthropicBash;
impl Extension for AnthropicBash {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::BeforeProviderRequest, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else { return Ok(EventResult::None) };
            let result = add_anthropic_bash_to_payload(ctx.model.as_ref().map(|model| model.api.as_str()), payload, is_anthropic_bash_enabled());
            Ok(EventResult::ProviderPayload(result))
        })));
        api.on(EventKind::BeforeAgentStart, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(EventResult::None) };
            if ctx.model.as_ref().is_none_or(|model| model.api != "anthropic-messages") || !is_anthropic_bash_enabled() { return Ok(EventResult::None); }
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                system_prompt: Some(format!("{}\n{}", event.system_prompt, ANTHROPIC_BASH_SECTION)),
                ..Default::default()
            }))
        })));
    }
}
