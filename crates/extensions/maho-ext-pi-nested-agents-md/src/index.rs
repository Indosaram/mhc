use crate::{inject_directory_context::{InjectionConfig, inject_directory_context}, injection_cache::InjectionCache, session_key::get_session_key};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, FlagType, FlagValue, ToolContent, ToolResultEventResult};
use std::{collections::{BTreeMap, BTreeSet}, path::Path, sync::{Arc, Mutex}};

#[derive(Default)]
struct State { cache: InjectionCache, disabled: bool, widget_visible: bool, files: BTreeMap<String, Vec<crate::reporter::InjectedFileMeta>>, errors: BTreeSet<String> }

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
                if !result.errors.is_empty() { state.errors.insert(session.clone()); }
                let has_errors = state.errors.contains(&session);
                crate::reporter::update_status(ctx,&state.cache,&session,has_errors);
                if result.injected_text.is_empty() { return Ok(EventResult::None); }
                let files = state.files.entry(session.clone()).or_default();
                for file in &result.injected_files {
                    let metadata = crate::reporter::InjectedFileMeta { absolute_path: file.absolute_path.clone(), truncated: file.truncated };
                    if let Some(existing) = files.iter_mut().find(|existing| existing.absolute_path == file.absolute_path) { *existing = metadata; }
                    else { files.push(metadata); }
                }
                if state.widget_visible { crate::reporter::update_widget(ctx,true,state.files.get(&session).map(Vec::as_slice).unwrap_or_default()); }
                let mut content = event.content.clone();
                content.push(ToolContent::text(result.injected_text));
                Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(content), details: None, is_error: None, usage: None }))
            })
        }));
        for kind in [EventKind::SessionCompact, EventKind::SessionShutdown] {
            let state = Arc::clone(&state);
            api.on(kind, Arc::new(move |_, ctx| {
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let session = get_session_key(ctx);
                state.cache.clear_session(&session);
                state.files.remove(&session);
                state.errors.remove(&session);
                if kind == EventKind::SessionCompact {
                    crate::reporter::update_status(ctx,&state.cache,&session,false);
                    if state.widget_visible { crate::reporter::update_widget(ctx,true,&[]); }
                }
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
        let command_api = Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        api.register_command("nested-agents",Some("Toggle the nested AGENTS.md context widget and dump cache state.".into()),None,Arc::new(move |_,ctx| {
            let state = Arc::clone(&state); let command_api = Arc::clone(&command_api);
            Box::pin(async move {
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.disabled { ctx.ui.notify("nested-agents-md is disabled via --no-nested-agents",maho_ext_api::NotificationType::Info); return Ok(()); }
                state.widget_visible = !state.widget_visible;
                let session = get_session_key(ctx);
                let files = state.files.get(&session).map(Vec::as_slice).unwrap_or_default();
                crate::reporter::update_widget(ctx,state.widget_visible,files);
                command_api.append_entry("nested-agents-md:debug",Some(crate::reporter::build_debug_record(&state.cache,&session,files)))?;
                ctx.ui.notify(if state.widget_visible { "Nested AGENTS.md context widget shown" } else { "Nested AGENTS.md context widget hidden" },maho_ext_api::NotificationType::Info);
                Ok(())
            })
        }));
    }
}
