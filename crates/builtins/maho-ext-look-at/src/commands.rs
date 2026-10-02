use std::sync::{Arc,Mutex};
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionFailure,NotificationType,ExtensionUiDialogOptions};
use maho_ai::{model::Model,types::InputModality};
use maho_core::model_resolver::parse_model_pattern;
use crate::{settings::{LookAtStore,load_look_at_chain,load_look_at_enabled},model_selector::resolve_vision_model};
pub type LookAtResync=Arc<dyn Fn(&ExtensionContext)->Result<(),ExtensionFailure>+Send+Sync>;
const MENU:[&str;4]=["Show current chain","Edit chain","Reset session override","Toggle look_at"];
pub fn parse_entries(raw:&str)->Vec<String> { raw.split_whitespace().map(String::from).collect() }
pub fn validate_entries(entries:&[String],available:&[Model])->Vec<String> {
    let vision:Vec<_>=available.iter().filter(|model|model.input.contains(&InputModality::Image)).cloned().collect();
    entries.iter().filter_map(|entry| {
        let parsed=parse_model_pattern(entry,&vision,true);
        if parsed.model.is_none() { Some(format!("No available image-capable model matches \"{entry}\"; saved for a future auth setup.")) } else { parsed.warning }
    }).collect()
}
fn render_state(ctx:&ExtensionContext,store:&LookAtStore)->Result<String,ExtensionFailure> {
    let settings=ctx.get_look_at_settings()?; let chain=load_look_at_chain(&settings,store);
    let available=ctx.model_registry.get_available();
    let vision:Vec<_>=available.iter().filter(|model|model.input.contains(&InputModality::Image)).cloned().collect();
    let source=if store.get_override().models.is_some() { "current-session override" } else if settings.models.is_some() { "settings.json lookAt.models" } else { "default" };
    let mut lines=vec![format!("look_at: {}",if load_look_at_enabled(&settings,store) { "enabled" } else { "disabled" }),format!("Model chain (source: {source}):")];
    if chain.is_empty() { lines.push("  (empty)".into()); }
    for entry in chain {
        let resolved=if parse_model_pattern(&entry,&vision,true).model.is_none() { "unavailable".into() } else { resolve_vision_model(std::slice::from_ref(&entry),&available).map(|r|format!("{}/{}",r.model.provider,r.model.id)).unwrap_or_else(||"unavailable".into()) };
        lines.push(format!("  {entry} -> {resolved}"));
    }
    lines.extend([String::new(),"Note: this override is current-session only; permanent config is settings.json lookAt.models.".into()]);
    Ok(lines.join("\n"))
}
fn save_chain(ctx:&ExtensionContext,entries:Vec<String>,store:&Mutex<LookAtStore>,resync:&LookAtResync)->Result<(),ExtensionFailure> {
    let warnings=validate_entries(&entries,&ctx.model_registry.get_available());
    if !warnings.is_empty() { ctx.ui.notify(&warnings.join("\n"),NotificationType::Warning); }
    store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_models(Some(entries));
    resync(ctx)?; ctx.ui.notify("look_at model chain saved for this session.",NotificationType::Info); Ok(())
}
pub fn register_look_at_command(api:&mut ExtensionApi,store:Arc<Mutex<LookAtStore>>,resync:LookAtResync) {
    api.register_command("lookat",Some("View and manage the current-session look_at vision model chain.".into()),Some("[model1 [model2 ...]]".into()),Arc::new(move |raw,ctx| {
        let store=Arc::clone(&store); let resync=Arc::clone(&resync);
        Box::pin(async move {
            let entries=parse_entries(raw);
            if !entries.is_empty() { return save_chain(ctx,entries,&store,&resync); }
            if !ctx.has_ui { ctx.ui.notify("look_at menu requires interactive UI. Use /lookat <model1> [model2 ...].",NotificationType::Error); return Ok(()); }
            let state=render_state(ctx,&store.lock().unwrap_or_else(std::sync::PoisonError::into_inner))?;
            let menu=MENU.map(String::from);
            let choice=ctx.ui.select(&state,&menu,ExtensionUiDialogOptions::default()).await;
            match choice.as_deref() {
                None|Some("")=>{},
                Some("Show current chain")=>{ctx.ui.notify(&render_state(ctx,&store.lock().unwrap_or_else(std::sync::PoisonError::into_inner))?,NotificationType::Info);},
                Some("Edit chain")=>{
                    let prefill=load_look_at_chain(&ctx.get_look_at_settings()?,&store.lock().unwrap_or_else(std::sync::PoisonError::into_inner)).join(" ");
                    if let Some(input)=ctx.ui.input("look_at model chain",Some(&prefill),ExtensionUiDialogOptions::default()).await {
                        let entries=parse_entries(&input);
                        if entries.is_empty() { ctx.ui.notify("Usage: /lookat <model1> [model2 ...]",NotificationType::Error); }
                        else { save_chain(ctx,entries,&store,&resync)?; }
                    }
                },
                Some("Reset session override")=>{
                    { let mut guard=store.lock().unwrap_or_else(std::sync::PoisonError::into_inner); guard.set_models(None); guard.set_enabled(None); }
                    resync(ctx)?; ctx.ui.notify("Reset look_at session override.",NotificationType::Info);
                },
                Some(_)=>{
                    let enabled=!load_look_at_enabled(&ctx.get_look_at_settings()?,&store.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                    store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_enabled(Some(enabled));
                    resync(ctx)?; ctx.ui.notify(&format!("look_at {} for this session.",if enabled { "enabled" } else { "disabled" }),NotificationType::Info);
                },
            }
            Ok(())
        })
    }));
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn whitespace_separates_entries() { assert_eq!(parse_entries("  one\n two\tthree  "),vec!["one","two","three"]); }
    #[test] fn unavailable_entries_are_saved_with_warning() { assert_eq!(validate_entries(&["missing".into()],&[]).len(),1); }
}
