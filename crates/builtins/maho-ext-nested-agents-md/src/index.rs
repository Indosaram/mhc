use std::{collections::{BTreeMap, BTreeSet}, path::Path, sync::{Arc, Mutex}};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, FlagType, FlagValue, NotificationType, SessionCompactEvent, ToolContent, ToolResultEventResult};
use crate::{core::{inject_directory_context::inject_directory_context, injection_cache::InjectionCache, session_key::get_session_key, types::{InjectedFileInfo, InjectionConfig}}, ui::reporter::{build_debug_record, clear_status, update_status, update_widget}};
#[derive(Default)]
struct State { cache: InjectionCache, files: BTreeMap<String, Vec<InjectedFileInfo>>, errors: BTreeSet<String>, widget_visible: bool, disabled: bool }
#[derive(Default)]
pub struct NestedAgentsMd;
impl Extension for NestedAgentsMd {
 fn register(&self, api: &mut ExtensionApi) {
  api.register_flag("no-nested-agents", FlagType::Boolean { default: Some(false) }, Some("Disable nested AGENTS.md context injection.".into()));
  let state = Arc::new(Mutex::new(State::default()));
  let runtime = api.runtime.clone();
  let shared = Arc::clone(&state);
  api.on(EventKind::SessionStart, Arc::new(move |_, ctx| {
   let shared = Arc::clone(&shared); let runtime = runtime.clone();
   Box::pin(async move {
    let mut state = shared.lock().map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
    state.disabled = runtime.get_flag("no-nested-agents") == Some(FlagValue::Boolean(true));
    if state.disabled { clear_status(ctx); update_widget(ctx, false, &[]); }
    Ok(EventResult::None)
   })
  }));
  let shared = Arc::clone(&state);
  api.on(EventKind::ToolResult, Arc::new(move |event, ctx| {
   let shared = Arc::clone(&shared);
   Box::pin(async move {
    let ExtensionEvent::ToolResult(event) = event else { return Ok(EventResult::None); };
    let mut state = shared.lock().map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
    if state.disabled || event.tool_name != "read" || event.is_error || !event.content.iter().any(|block| matches!(block, ToolContent::Text { .. })) { return Ok(EventResult::None); }
    let Some(path) = event.input.get("path").and_then(serde_json::Value::as_str).filter(|path| !path.is_empty()) else { return Ok(EventResult::None); };
    let key = get_session_key(ctx);
    let result = inject_directory_context(Path::new(path), &ctx.cwd, &mut state.cache, &key, &InjectionConfig::default());
    if !result.errors.is_empty() { state.errors.insert(key.clone()); }
    update_status(ctx, &state.cache, &key, state.errors.contains(&key));
    if result.injected_text.is_empty() { return Ok(EventResult::None); }
    let files = state.files.entry(key).or_default();
    for file in result.injected_files { if !files.iter().any(|existing| existing.absolute_path == file.absolute_path) { files.push(file); } }
    if state.widget_visible { update_widget(ctx, true, state.files.get(&get_session_key(ctx)).map_or(&[], Vec::as_slice)); }
    let mut content = event.content.clone();
    content.push(ToolContent::Text { text: result.injected_text, audience: Some("model".into()) });
    Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(content), ..Default::default() }))
   })
  }));
  for kind in [EventKind::SessionCompact, EventKind::SessionShutdown] {
   let shared = Arc::clone(&state);
   api.on(kind, Arc::new(move |event, ctx| {
    let shared = Arc::clone(&shared);
    Box::pin(async move {
     if matches!(event, ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected { .. })) { return Ok(EventResult::None); }
     let mut state = shared.lock().map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
     let key = get_session_key(ctx); state.cache.clear_session(&key); state.files.remove(&key); state.errors.remove(&key);
     if kind == EventKind::SessionCompact { update_status(ctx, &state.cache, &key, false); if state.widget_visible { update_widget(ctx, true, &[]); } }
     Ok(EventResult::None)
    })
   }));
  }
  let actions = Arc::new(ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
  api.register_command("nested-agents", Some("Toggle the nested AGENTS.md context widget and dump cache state.".into()), None, Arc::new(move |_, ctx| {
   let shared = Arc::clone(&state); let actions = Arc::clone(&actions);
   Box::pin(async move {
    let mut state = shared.lock().map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
    if state.disabled { ctx.ui.notify("nested-agents-md is disabled via --no-nested-agents", NotificationType::Info); return Ok(()); }
    state.widget_visible = !state.widget_visible;
    let key = get_session_key(ctx); let files: &[InjectedFileInfo] = state.files.get(&key).map_or(&[], Vec::as_slice);
    update_widget(ctx, state.widget_visible, files);
    actions.append_entry("nested-agents-md:debug", Some(build_debug_record(&state.cache, &key, files)))?;
    ctx.ui.notify(if state.widget_visible { "Nested AGENTS.md context widget shown" } else { "Nested AGENTS.md context widget hidden" }, NotificationType::Info);
    Ok(())
   })
  }));
 }
}
