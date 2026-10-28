use maho_ext_api::{BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionWidgetOptions, Model};
use serde_json::{Value, json};
use std::sync::Arc;

pub const OPENAI_WEB_SEARCH_SECTION: &str = "\n## Web Search\n\nUse the available web search tool when the user asks for current or online information.\nPrefer web search over guessing when freshness matters.\n";
const SOURCES: &str = "web_search_call.action.sources";
const SERVER_SEARCH_FALLBACK: &str = "server_search_fallback";
const SERVER_SEARCH_DISABLED: &str = "server_search_disabled";

fn server_search_suppressed(payload: &Value) -> bool {
    [SERVER_SEARCH_FALLBACK, "__senpi_server_search_fallback", SERVER_SEARCH_DISABLED, "__senpi_server_search_disabled"]
        .iter().any(|key| payload.get(*key).and_then(Value::as_bool) == Some(true))
}

pub fn parse_enabled(value: Option<&str>) -> bool {
    !value.is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
}
fn enabled() -> bool { parse_enabled(std::env::var("PI_OPENAI_WEB_SEARCH").ok().as_deref()) }
fn responses(model: Option<&Model>) -> bool { model.is_some_and(|model| matches!(model.api.as_str(), "openai-responses" | "azure-openai-responses" | "openai-codex-responses")) }
pub fn supports_native_openai_web_search(model: Option<&Model>) -> bool {
    let Some(model) = model.filter(|_| responses(model)) else { return false };
    if model.api == "azure-openai-responses" { return true; }
    if model.api == "openai-codex-responses" {
        if model.id.contains("-spark") { return false; }
        return model.compat.as_ref().and_then(|compat| compat.get("supportsWebSearchPreview")).and_then(Value::as_bool)
            .unwrap_or_else(|| url::Url::parse(&model.base_url).ok().is_some_and(|url| url.host_str() == Some("chatgpt.com")));
    }
    model.compat.as_ref().and_then(|compat| compat.get("supportsWebSearchPreview")).and_then(Value::as_bool)
        .unwrap_or_else(|| url::Url::parse(if model.base_url.is_empty() { "https://api.openai.com/v1" } else { &model.base_url }).ok().is_some_and(|url| url.host_str() == Some("api.openai.com")))
}
fn native(tool: &Value) -> bool { matches!(tool.get("type").and_then(Value::as_str), Some("web_search" | "web_search_preview" | "web_search_preview_2025_03_11")) }
fn strip(payload: &Value) -> Value {
    if !payload.is_object() { return payload.clone(); }
    let mut result = payload.clone();
    if let Some(tools) = payload.get("tools").and_then(Value::as_array) { result["tools"] = json!(tools.iter().filter(|tool| !native(tool)).collect::<Vec<_>>()); }
    if let Some(include) = payload.get("include").and_then(Value::as_array) { result["include"] = json!(include.iter().filter(|value| value.as_str() != Some(SOURCES)).collect::<Vec<_>>()); }
    if payload.get("tool_choice").is_some_and(native) && let Some(object) = result.as_object_mut() { object.remove("tool_choice"); }
    result
}
fn sanitize(tools: &[Value], strip_function: bool) -> Vec<Value> {
    tools.iter().filter(|tool| {
        if !tool.is_object() { return false; }
        let kind = tool.get("type").and_then(Value::as_str);
        let unsupported = kind.is_some_and(|kind| ((kind == "web_search" || kind.starts_with("web_search_")) && !native(tool)) || kind.starts_with("web_fetch_"));
        let function = strip_function && tool.get("name").or_else(|| tool.get("function").and_then(|function| function.get("name"))).and_then(Value::as_str) == Some("web_search") && !native(tool);
        !unsupported && !function
    }).cloned().collect()
}
fn needs_strip(payload: &Value) -> bool {
    payload.is_object()
        && (payload.get("tools").and_then(Value::as_array).is_some_and(|tools| tools.iter().any(native))
            || payload.get("include").and_then(Value::as_array).is_some_and(|include| include.iter().any(|value| value.as_str() == Some(SOURCES)))
            || payload.get("tool_choice").is_some_and(native))
}
/// `None` when `add_openai_web_search_to_payload` would return the payload unchanged, so callers can skip copying it.
pub fn add_openai_web_search_to_payload_if_changed(model: Option<&Model>, payload: &Value, enabled: bool) -> Option<Value> {
    if server_search_suppressed(payload) {
        return needs_strip(payload).then(|| strip(payload));
    }
    if !responses(model) { return needs_strip(payload).then(|| strip(payload)); }
    if !payload.is_object() { return None; }
    let supports = supports_native_openai_web_search(model);
    if !(supports && enabled) && (supports || !needs_strip(payload)) {
        let active = payload.get("tools").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        if sanitize(active, false) == active { return None; }
    }
    Some(add_openai_web_search_to_payload(model, payload, enabled))
}
pub fn add_openai_web_search_to_payload(model: Option<&Model>, payload: &Value, enabled: bool) -> Value {
    if server_search_suppressed(payload) {
        return strip(payload);
    }
    if !responses(model) { return strip(payload); }
    if !payload.is_object() { return payload.clone(); }
    let supports = supports_native_openai_web_search(model);
    let mut result = if supports { payload.clone() } else { strip(payload) };
    let active = result.get("tools").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut tools = sanitize(active, supports && enabled);
    if !(supports && enabled) {
        if tools != active { result["tools"] = json!(tools); }
        return result;
    }
    if !tools.iter().any(native) { tools.push(json!({"type":if model.is_some_and(|model| model.api == "openai-codex-responses") { "web_search" } else { "web_search_preview" }})); }
    let mut include: Vec<Value> = payload.get("include").and_then(Value::as_array).into_iter().flatten().filter(|value| value.is_string()).cloned().collect();
    if !include.iter().any(|value| value.as_str() == Some(SOURCES)) { include.push(json!(SOURCES)); }
    result["tools"] = json!(tools);
    result["include"] = json!(include);
    result
}
pub struct OpenAiWebSearch;
impl Extension for OpenAiWebSearch {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::BeforeProviderRequest, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeProviderRequest { payload, model, .. } = event else { return Ok(EventResult::None) };
            Ok(add_openai_web_search_to_payload_if_changed(model.as_ref().or(ctx.model.as_ref()), payload, enabled()).map_or(EventResult::None, EventResult::ProviderPayload))
        })));
        for kind in [EventKind::SessionStart, EventKind::ModelSelect, EventKind::SessionShutdown] {
            api.on(kind, Arc::new(|_event, ctx| Box::pin(async move {
                if ctx.has_ui {
                    ctx.ui.set_status("openai-web-search", None);
                    ctx.ui.set_widget("openai-web-search", None, ExtensionWidgetOptions::default());
                }
                Ok(EventResult::None)
            })));
        }
        api.on(EventKind::BeforeAgentStart, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(EventResult::None) };
            if !supports_native_openai_web_search(ctx.model.as_ref()) || !enabled() { return Ok(EventResult::None); }
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                system_prompt: Some(format!("{}\n{}", event.system_prompt, OPENAI_WEB_SEARCH_SECTION)), ..Default::default()
            }))
        })));
    }
}
