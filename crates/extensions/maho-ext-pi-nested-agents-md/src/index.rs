use crate::{inject_directory_context::{InjectionConfig, inject_directory_context}, injection_cache::InjectionCache, session_key::get_session_key};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, FlagType, FlagValue, ToolContent, ToolResultEventResult};
use std::{path::Path, sync::{Arc, Mutex}};

#[derive(Default)]
struct State { cache: InjectionCache, disabled: bool }

pub struct NestedAgentsMd;
impl Extension for NestedAgentsMd {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag("no-nested-agents", FlagType::Boolean { default: Some(false) }, Some("Disable nested AGENTS.md context injection.".into()));
        let state = Arc::new(Mutex::new(State::default()));
        let runtime = api.runtime.clone();
        let start_state = Arc::clone(&state);
        api.on(EventKind::SessionStart, Arc::new(move |_, ctx| {
            let disabled = runtime.get_flag("no-nested-agents") == Some(FlagValue::Boolean(true));
            start_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disabled = disabled;
            Box::pin(async move {
                if disabled && ctx.has_ui {
                    ctx.ui.set_status("ext:nested-agents:status", None);
                    ctx.ui.set_widget("ext:nested-agents:widget", None, Default::default());
                }
                Ok(EventResult::None)
            })
        }));
        let read_state = Arc::clone(&state);
        api.on(EventKind::ToolResult, Arc::new(move |event, ctx| {
            let state = Arc::clone(&read_state);
            Box::pin(async move {
                let ExtensionEvent::ToolResult(event) = event else { return Ok(EventResult::None); };
                if event.tool_name != "read" || event.is_error || !event.content.iter().any(|block| matches!(block, ToolContent::Text { .. })) { return Ok(EventResult::None); }
                let Some(path) = event.input.get("path").and_then(|value| value.as_str()).filter(|path| !path.is_empty()) else { return Ok(EventResult::None); };
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.disabled { return Ok(EventResult::None); }
                let session = get_session_key(ctx);
                let result = inject_directory_context(Path::new(path), &ctx.cwd, &mut state.cache, &session, &InjectionConfig::default());
                if result.injected_text.is_empty() { return Ok(EventResult::None); }
                let mut content = event.content.clone();
                content.push(ToolContent::text(result.injected_text));
                Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(content), details: None, is_error: None, usage: None }))
            })
        }));
        for kind in [EventKind::SessionCompact, EventKind::SessionShutdown] {
            let state = Arc::clone(&state);
            api.on(kind, Arc::new(move |_, ctx| {
                state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cache.clear_session(&get_session_key(ctx));
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
    }
}
