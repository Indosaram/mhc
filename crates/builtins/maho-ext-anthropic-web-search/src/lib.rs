use maho_ext_api::{BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionWidgetOptions, Model};
use serde_json::{Value, json};
use std::sync::Arc;

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
    if model.is_none_or(|model| model.api != "anthropic-messages") || !payload.is_object() { return payload.clone(); }
    let mut result = payload.clone();
    if !supports_native_anthropic_web_search(model) {
        let Some(tools) = payload.get("tools").and_then(Value::as_array) else { return result };
        let kept: Vec<Value> = tools.iter().filter(|tool| !native(tool)).cloned().collect();
        if kept.len() == tools.len() { return result; }
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
        return result;
    }
    if !options.enabled { return result; }
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
    result
}

pub struct AnthropicWebSearch;
impl Extension for AnthropicWebSearch {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::BeforeProviderRequest, Arc::new(|event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeProviderRequest { payload, .. } = event else { return Ok(EventResult::None) };
            Ok(EventResult::ProviderPayload(add_anthropic_web_search_to_payload(ctx.model.as_ref(), payload, &SearchOptions::from_env())))
        })));
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
