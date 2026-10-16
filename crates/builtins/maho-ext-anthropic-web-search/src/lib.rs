use maho_ext_api::{BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionWidgetOptions, Model};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub const ANTHROPIC_WEB_SEARCH_SECTION: &str = "\n## Web Search\n\nThe native web_search tool is available in this session.\nUse web_search when the user asks for current or online information.\nPrefer web_search over guessing when freshness matters.\n";

pub fn parse_enabled(value: Option<&str>) -> bool {
    !value.is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
}

pub fn supports_native_anthropic_web_search(model: Option<&Model>) -> bool {
    let Some(model) = model.filter(|model| model.api == "anthropic-messages") else { return false };
    model.compat.as_ref().and_then(|compat| compat.get("supportsWebSearch")).and_then(Value::as_bool)
        .unwrap_or_else(|| url::Url::parse(&model.base_url).ok().is_some_and(|url| url.host_str() == Some("api.anthropic.com")))
}

fn native(tool: &Value) -> bool {
    tool.get("type").and_then(Value::as_str).is_some_and(|kind| kind.starts_with("web_search_"))
}

#[derive(Default)]
pub struct SearchOptions {
    pub enabled: bool,
    pub allowed_domains: Option<String>,
    pub blocked_domains: Option<String>,
}
impl SearchOptions {
    fn from_env() -> Self {
        Self { enabled: parse_enabled(std::env::var("PI_ANTHROPIC_WEB_SEARCH").ok().as_deref()),
            allowed_domains: std::env::var("PI_ANTHROPIC_WEB_SEARCH_ALLOWED_DOMAINS").ok(),
            blocked_domains: std::env::var("PI_ANTHROPIC_WEB_SEARCH_BLOCKED_DOMAINS").ok() }
    }
}

pub fn add_anthropic_web_search_to_payload(model: Option<&Model>, payload: &Value, options: &SearchOptions) -> Value {
    add_anthropic_web_search_to_payload_if_changed(model, payload, options).unwrap_or_else(|| payload.clone())
}

pub const SERVER_SEARCH_FALLBACK_KEY: &str = "__senpi_server_search_fallback";

pub fn mark_server_search_fallback(payload: &Value) -> Value {
    if !payload.is_object() { return payload.clone(); }
    let mut result = payload.clone();
    result[SERVER_SEARCH_FALLBACK_KEY] = Value::Bool(true);
    result
}

pub fn is_server_search_fallback(payload: &Value) -> bool { payload.get(SERVER_SEARCH_FALLBACK_KEY).and_then(Value::as_bool) == Some(true) }

fn model_key(model: &Model) -> String { format!("{}\0{}\0{}\0{}", model.api, model.provider, model.id, model.base_url) }

fn strip_native_web_search(payload: &Value) -> Option<Value> {
    let tools = payload.get("tools").and_then(Value::as_array)?;
    if !tools.iter().any(native) { return Some(payload.clone()); }
    let mut result = payload.clone();
    let kept: Vec<Value> = tools.iter().filter(|tool| !native(tool)).cloned().collect();
    if kept.is_empty() {
        if let Some(object) = result.as_object_mut() { object.remove("tools"); }
    } else { result["tools"] = json!(kept); }
    if let Some(choice) = payload.get("tool_choice").filter(|choice| choice.is_object()) {
        let selected = choice.get("name").and_then(Value::as_str);
        if (kept.is_empty() || selected.is_some_and(|name| !kept.iter().any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))))
            && let Some(object) = result.as_object_mut() { object.remove("tool_choice"); }
    }
    Some(result)
}

/// `None` when the payload is left untouched, so callers can skip copying it.
pub fn add_anthropic_web_search_to_payload_if_changed(model: Option<&Model>, payload: &Value, options: &SearchOptions) -> Option<Value> {
    if model.is_none_or(|model| model.api != "anthropic-messages") || !payload.is_object() { return None; }
    let marked_fallback = is_server_search_fallback(payload);
    if marked_fallback {
        let mut cleaned = payload.clone();
        if let Some(object) = cleaned.as_object_mut() { object.remove(SERVER_SEARCH_FALLBACK_KEY); }
        return strip_native_web_search(&cleaned);
    }
    if !supports_native_anthropic_web_search(model) {
        let tools = payload.get("tools").and_then(Value::as_array)?;
        if !tools.iter().any(native) { return None; }
        let mut result = payload.clone();
        let kept: Vec<Value> = tools.iter().filter(|tool| !native(tool)).cloned().collect();
        if let Some(choice) = payload.get("tool_choice").filter(|choice| choice.is_object()) {
            let selected = choice.get("name").and_then(Value::as_str);
            if (kept.is_empty() || selected.is_some_and(|name| !kept.iter().any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))))
                && let Some(object) = result.as_object_mut() {
                object.remove("tool_choice");
            }
        }
        if kept.is_empty() {
            if let Some(object) = result.as_object_mut() { object.remove("tools"); }
        } else { result["tools"] = json!(kept); }
        return Some(result);
    }
    if !options.enabled { return None; }
    let mut result = payload.clone();
    let mut tools: Vec<Value> = payload.get("tools").and_then(Value::as_array).into_iter().flatten()
        .filter(|tool| tool.is_object() && (tool.get("name").and_then(Value::as_str) != Some("web_search") || native(tool)))
        .cloned().collect();
    if !tools.iter().any(native) {
        let mut tool = json!({"type":"web_search_20250305","name":"web_search","max_uses":8});
        for (key, value) in [("allowed_domains", &options.allowed_domains), ("blocked_domains", &options.blocked_domains)] {
            if let Some(value) = value {
                let domains: Vec<&str> = value.split(',').map(str::trim).filter(|domain| !domain.is_empty()).collect();
                if !domains.is_empty() { tool[key] = json!(domains); }
            }
        }
        tools.push(tool);
    }
    result["tools"] = json!(tools);
    Some(result)
}

pub struct AnthropicWebSearch;
impl Extension for AnthropicWebSearch {
    fn register(&self, api: &mut ExtensionApi) {
        let failed_models = Arc::new(Mutex::new(HashSet::new()));
        let request_failed_models = Arc::clone(&failed_models);
        api.on(EventKind::BeforeProviderRequest, Arc::new(move |event, ctx| {
            let failed_models = Arc::clone(&request_failed_models);
            Box::pin(async move {
            let ExtensionEvent::BeforeProviderRequest { payload, model, .. } = event else { return Ok(EventResult::None) };
            let model = model.as_ref().or(ctx.model.as_ref());
            let failed = model.is_some_and(|model| failed_models.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&model_key(model)));
            let payload = if failed { mark_server_search_fallback(payload) } else { payload.clone() };
            Ok(add_anthropic_web_search_to_payload_if_changed(model, &payload, &SearchOptions::from_env()).map_or(EventResult::None, EventResult::ProviderPayload))
        })
        }));
        api.on(EventKind::MessageEnd, Arc::new(move |event, ctx| {
            let failed_models = Arc::clone(&failed_models);
            Box::pin(async move {
            let ExtensionEvent::MessageEnd { message: maho_ext_api::AgentMessage::Llm(message) } = event else { return Ok(EventResult::None) };
            let maho_ai::types::Message::Assistant(assistant) = message else { return Ok(EventResult::None) };
            if !has_server_search_failure(assistant) { return Ok(EventResult::None); }
            if let Some(model) = ctx.model.as_ref().filter(|model| model.api == assistant.api && model.provider == assistant.provider && model.id == assistant.model) {
                failed_models.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(model_key(model));
            }
            Ok(EventResult::None)
        })
        }));
        for kind in [EventKind::SessionStart, EventKind::ModelSelect, EventKind::SessionShutdown] {
            api.on(kind, Arc::new(|_event, ctx| Box::pin(async move {
                if ctx.has_ui {
                    ctx.ui.set_status("anthropic-web-search", None);
                    ctx.ui.set_widget("anthropic-web-search", None, ExtensionWidgetOptions::default());
                }
                Ok(EventResult::None)
            })));
        }
        api.on(EventKind::BeforeAgentStart, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(EventResult::None) };
            if !supports_native_anthropic_web_search(ctx.model.as_ref()) || !SearchOptions::from_env().enabled { return Ok(EventResult::None); }
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                system_prompt: Some(format!("{}\n{}", event.system_prompt, ANTHROPIC_WEB_SEARCH_SECTION)), ..Default::default()
            }))
        })));
    }
}

fn has_server_search_failure(message: &maho_ai::types::AssistantMessage) -> bool {
    message.error_message.as_deref().is_some_and(|error| error.contains("web_search_tool_result_error"))
        || message.content.iter().any(|block| match block {
            maho_ai::types::ContentBlock::ProviderNative(native) => native.subtype == "web_search_tool_result_error" || native.subtype == "web_search_tool_result" && native.raw.get("content").and_then(Value::as_array).is_some_and(|items| items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("web_search_tool_result_error"))),
            maho_ai::types::ContentBlock::Text(text) => text.text.contains("web_search_tool_result_error"),
            _ => false,
        })
}
