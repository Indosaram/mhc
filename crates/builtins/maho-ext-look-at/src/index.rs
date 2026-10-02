use maho_ai::{model::Model,types::InputModality};
pub fn register_activation_hooks(api:&mut maho_ext_api::ExtensionApi,store:std::sync::Arc<std::sync::Mutex<crate::settings::LookAtStore>>) {
    let runtime=api.runtime.clone();
    for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::ModelSelect] {
        let runtime=runtime.clone(); let store=store.clone();
        api.on(event,std::sync::Arc::new(move |_event,ctx| {
            let runtime=runtime.clone(); let store=store.clone();
            Box::pin(async move {
                sync_tool_activation(&runtime,ctx,&store.lock().unwrap_or_else(std::sync::PoisonError::into_inner))?;
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}
pub fn sync_tool_activation(runtime:&maho_ext_api::ExtensionRuntime,ctx:&maho_ext_api::ExtensionContext,store:&crate::settings::LookAtStore)->Result<(),maho_ext_api::ExtensionFailure> {
    let actions=runtime.session_actions()?;
    let active=actions.get_active_tools()?;
    let enabled=match store.get_override().enabled {Some(enabled)=>enabled,None=>ctx.get_look_at_settings()?.enabled};
    let vision_available=if enabled && ctx.model.as_ref().is_some_and(|model|!model.input.contains(&InputModality::Image)) {
        let chain=match &store.get_override().models {Some(models)=>models.clone(),None=>ctx.get_look_at_settings()?.models.unwrap_or_else(||crate::model_selector::DEFAULT_LOOK_AT_CHAIN.map(String::from).to_vec())};
        crate::model_selector::resolve_vision_model(&chain,&ctx.model_registry.get_available()).is_some()
    } else { false };
    if let Some(next)=tool_activation(enabled,ctx.model.as_ref(),vision_available,&active) { actions.set_active_tools(next)?; }
    Ok(())
}
pub fn tool_activation(enabled:bool,model:Option<&Model>,vision_model_available:bool,active:&[String])->Option<Vec<String>> {
    let should_be_active=enabled && model.is_some_and(|model|!model.input.contains(&InputModality::Image)) && vision_model_available;
    let is_active=active.iter().any(|name|name=="look_at");
    if should_be_active && !is_active { let mut next=active.to_vec(); next.push("look_at".into()); Some(next) }
    else if !should_be_active && is_active { Some(active.iter().filter(|name|name.as_str()!="look_at").cloned().collect()) } else { None }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_activation_hooks_register_start_and_model_select() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("look-at",std::path::PathBuf::new(),Default::default()),Default::default(),Default::default(),Default::default());
        register_activation_hooks(&mut api,std::sync::Arc::new(std::sync::Mutex::new(crate::settings::create_look_at_store())));
        assert_eq!(api.registered.handlers.len(),2);
        assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::SessionStart].len(),1);
        assert_eq!(api.registered.handlers[&maho_ext_api::EventKind::ModelSelect].len(),1);
    }
    #[test] fn absent_model_and_disabled_setting_remove_tool() { let active=["read".into(),"look_at".into()]; assert_eq!(tool_activation(true,None,true,&active),Some(vec!["read".into()])); assert_eq!(tool_activation(false,None,true,&active),Some(vec!["read".into()])); assert_eq!(tool_activation(true,None,true,&["read".into()]),None); }
}
