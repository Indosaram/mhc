use serde_json::Value;
use crate::types::ApplyPatchWireMode;
pub use maho_ai::apply_patch_wire::{get_apply_patch_wire_mode, is_openai_gpt_model};
pub fn without_apply_patch(names:&[String])->Vec<String> { names.iter().filter(|name|name.as_str()!="apply_patch").cloned().collect() }
pub fn replace_edit_tools_with_apply_patch(names:&[String])->Vec<String> {
    let insert=names.iter().position(|name|matches!(name.as_str(),"write"|"edit"|"apply_patch"));
    let mut filtered:Vec<_>=names.iter().filter(|name|!matches!(name.as_str(),"write"|"edit"|"apply_patch")).cloned().collect();
    if let Some(index)=insert { filtered.insert(index.min(filtered.len()),"apply_patch".into()); } filtered
}
pub fn has_apply_patch_failures(details:&Value)->bool { details.get("result").and_then(|result|result.get("failures")).and_then(Value::as_array).is_some_and(|failures|!failures.is_empty()) }
pub fn register_failure_hook(api:&mut maho_ext_api::ExtensionApi) {
    api.on(maho_ext_api::EventKind::ToolResult,std::sync::Arc::new(|event,_context|Box::pin(async move { Ok(failure_hook_result(event)) })));
}
fn failure_hook_result(event:&maho_ext_api::ExtensionEvent)->maho_ext_api::EventResult {
    match event {
        maho_ext_api::ExtensionEvent::ToolResult(event) if event.tool_name=="apply_patch" && !event.is_error && event.details.as_ref().is_some_and(has_apply_patch_failures)=>maho_ext_api::EventResult::ToolResult(maho_ext_api::ToolResultEventResult{is_error:Some(true),..Default::default()}),
        _=>maho_ext_api::EventResult::None,
    }
}
pub fn sync_tool_names(mode:ApplyPatchWireMode,current:&[String],registered:&[String],removed_edit_tools:&mut Vec<String>)->Option<Vec<String>> {
    if mode!=ApplyPatchWireMode::None {
        let active:Vec<_>=current.iter().filter(|name|matches!(name.as_str(),"write"|"edit")).cloned().collect();
        if !active.is_empty() { *removed_edit_tools=active; }
        return Some(replace_edit_tools_with_apply_patch(current));
    }
    if removed_edit_tools.is_empty() { return current.iter().any(|name|name=="apply_patch").then(||without_apply_patch(current)); }
    let mut restored=without_apply_patch(current);
    restored.extend(removed_edit_tools.iter().filter(|name|registered.contains(name)).cloned()); removed_edit_tools.clear();
    let mut unique=Vec::new(); for name in restored { if !unique.contains(&name) { unique.push(name); } } Some(unique)
}

#[derive(Default)]
pub struct ApplyPatchExtension;

impl maho_ext_api::Extension for ApplyPatchExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        register_apply_patch_extension(api);
    }
}

fn register_native_tool(api: &mut maho_ext_api::ExtensionApi, mode: ApplyPatchWireMode) -> Result<(), maho_ext_api::ExtensionFailure> {
    let definition = crate::tool::create_apply_patch_tool_variant(mode);
    let execute = definition.execute.clone();
    api.register_tool_with_renderers(definition.clone(), crate::render::renderers())?;
    let scope = api.runtime.registration_scope();
    let mut executor_api = maho_ext_api::ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), scope.clone());
    executor_api.register_tool_with_extension_context(definition, std::sync::Arc::new(move |id, params, _, update, context| {
        let execute = execute.clone();
        Box::pin(async move {
            let on_update = update.map(|update| std::sync::Arc::new(move |result: maho_ext_api::ToolResult| {
                update(maho_ext_api::AgentToolResult {
                    content: result.content.into_iter().filter_map(|part| match part {
                        maho_ext_api::ToolContent::Text { text, .. } => Some(maho_ext_api::ContentBlock::text(text)),
                        maho_ext_api::ToolContent::Image { .. } => None,
                    }).collect(),
                    details: result.details.unwrap_or(Value::Null), ..maho_ext_api::AgentToolResult::text("")
                });
                Ok(())
            }) as maho_tools::definition::ToolUpdateCallback);
            let result = execute(maho_ext_api::ToolCall {
                id, params, signal: context.signal.clone().unwrap_or_default(), context: Some(context), on_update,
            }).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
            let details = result.details.unwrap_or(Value::Null);
            let is_error = has_apply_patch_failures(&details);
            Ok(maho_ext_api::AgentToolResult {
                content: result.content.into_iter().filter_map(|part| match part {
                    maho_ext_api::ToolContent::Text { text, .. } => Some(maho_ext_api::ContentBlock::text(text)),
                    maho_ext_api::ToolContent::Image { .. } => None,
                }).collect(),
                details, is_error: Some(is_error), ..maho_ext_api::AgentToolResult::text("")
            })
        })
    }))?;
    scope.commit_registration()
}

pub fn register_apply_patch_extension(api: &mut maho_ext_api::ExtensionApi) {
    use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent};
    use std::sync::{Arc, Mutex};
    if let Err(error) = register_native_tool(api, ApplyPatchWireMode::Freeform) { std::panic::panic_any(error); }
    register_failure_hook(api);
    let live = Arc::new(Mutex::new(ExtensionApi::new(
        api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone(),
    )));
    let state = Arc::new(Mutex::new((ApplyPatchWireMode::Freeform, ApplyPatchWireMode::None, Vec::new())));
    let runtime = api.runtime.clone();
    let lazy_state = state.clone();
    api.register_lazy_tool_activator(Arc::new(move |name| {
        if name != "apply_patch" || lazy_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 == ApplyPatchWireMode::None {
            return false;
        }
        let Ok(actions) = runtime.session_actions() else { return false; };
        let Ok(mut names) = actions.get_active_tools() else { return false; };
        if names.iter().any(|name| name == "apply_patch") { return false; }
        names.push("apply_patch".into());
        actions.set_active_tools(names).is_ok()
    }));
    for kind in [EventKind::SessionStart, EventKind::ModelSelect] {
        let live = live.clone();
        let state = state.clone();
        api.on(kind, Arc::new(move |event, context| {
            let live = live.clone();
            let state = state.clone();
            Box::pin(async move {
                let model = match event {
                    ExtensionEvent::ModelSelect(event) => Some(&event.model),
                    _ => context.model.as_ref(),
                };
                let mode = get_apply_patch_wire_mode(model.map(|model| (model.api.as_str(), model.id.as_str())));
                let mut api = live.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.1 = mode;
                let current = api.get_active_tools()?;
                if mode != ApplyPatchWireMode::None && state.0 != mode {
                    register_native_tool(&mut api, mode)?;
                    state.0 = mode;
                }
                let registered = if mode == ApplyPatchWireMode::None && !state.2.is_empty() {
                    api.get_all_tools()?.into_iter().map(|tool| tool.name).collect()
                } else { Vec::new() };
                if let Some(names) = sync_tool_names(mode, &current, &registered, &mut state.2) {
                    api.set_active_tools(names)?;
                }
                Ok(EventResult::None)
            })
        }));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_failure_hook_only_marks_nonerror_patch_failures() {
        for (name,is_error,failures,expected) in [("apply_patch",false,true,true),("apply_patch",true,true,false),("other",false,true,false),("apply_patch",false,false,false)] {
            let event=maho_ext_api::ExtensionEvent::ToolResult(maho_ext_api::ToolResultEvent{tool_call_id:"test".into(),tool_name:name.into(),input:Value::Null,content:vec![],details:Some(serde_json::json!({"result":{"failures":if failures {vec![Value::Null]} else {vec![]}}})),is_error,usage:None});
            match failure_hook_result(&event) { maho_ext_api::EventResult::ToolResult(result)=>{assert!(expected); assert_eq!(result.is_error,Some(true)); assert!(result.content.is_none()); assert!(result.details.is_none());},maho_ext_api::EventResult::None=>assert!(!expected),_=>panic!("unexpected hook result") }
        }
    }
    #[test] fn model_switch_restores_only_registered_edit_tools() { let current=["read","write","edit"].map(String::from); let mut removed=vec![]; let patched=sync_tool_names(ApplyPatchWireMode::Freeform,&current,&current,&mut removed).unwrap(); assert_eq!(patched,["read","apply_patch"]); let restored=sync_tool_names(ApplyPatchWireMode::None,&patched,&["read".into(),"edit".into()],&mut removed).unwrap(); assert_eq!(restored,["read","edit"]); assert!(removed.is_empty()); }
    #[test] fn no_mode_no_removed_tools_leaves_active_list_untouched() { assert_eq!(sync_tool_names(ApplyPatchWireMode::None,&["read".into()],&[],&mut vec![]),None); }
    #[test] fn gateway_gpt_ids_use_api_gate() { for id in ["codex/gpt-6-astra","global.openai.gpt-6-astra","gateway:GPT_6_ASTRA","gpt5"] { assert_eq!(get_apply_patch_wire_mode(Some(("openai-responses",id))),ApplyPatchWireMode::Freeform); assert_eq!(get_apply_patch_wire_mode(Some(("openai-completions",id))),ApplyPatchWireMode::Json); } }
    #[test] fn false_gpt_substrings_are_rejected() { for id in ["xgpt-5.6-proxy","deepseek-v3-gptq","gpt"] { assert_eq!(get_apply_patch_wire_mode(Some(("openai-responses",id))),ApplyPatchWireMode::None); } assert!(!is_openai_gpt_model(Some(("anthropic-messages","gpt-5")))); }
    #[test] fn replacement_preserves_first_edit_position() { let names=["read","write","bash","edit","apply_patch"].map(String::from); assert_eq!(replace_edit_tools_with_apply_patch(&names),["read","apply_patch","bash"]); assert_eq!(replace_edit_tools_with_apply_patch(&["read".into()]),["read"]); }
    #[test] fn malformed_failures_do_not_claim_partial_application() { assert!(!has_apply_patch_failures(&serde_json::json!({"result":{"failures":true}}))); assert!(has_apply_patch_failures(&serde_json::json!({"result":{"failures":[{}]}}))); }
}
