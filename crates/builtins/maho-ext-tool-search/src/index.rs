use crate::engine::document::{ToolSearchDocument,ToolSearchSource};
pub struct ToolSearchExtension {pub actions:std::sync::Arc<dyn maho_ext_api::ExtensionActions>,pub mcp_native_enabled:std::sync::Arc<dyn Fn()->bool+Send+Sync>}
impl maho_ext_api::Extension for ToolSearchExtension {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        use std::sync::{Arc,Mutex};
        let service=Arc::new(Mutex::new(crate::service::ToolSearchService::new(api.runtime.clone(),self.actions.clone())));
        let registration=Arc::new(Mutex::new(maho_ext_api::ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone())));
        let weak=Arc::downgrade(&service);let registered=Arc::new(std::sync::atomic::AtomicBool::new(false));
        service.lock().unwrap_or_else(std::sync::PoisonError::into_inner).bind_tool_registrar(Arc::new(move ||{
            if registered.load(std::sync::atomic::Ordering::SeqCst){return Ok(());}
            let service=weak.upgrade().ok_or_else(||maho_ext_api::ExtensionFailure::new("Tool-search service disposed"))?;
            registration.lock().unwrap_or_else(std::sync::PoisonError::into_inner).register_tool_with_renderers(crate::tool::create_tool_search_tool(service),crate::tool::renderers())?;
            registered.store(true,std::sync::atomic::Ordering::SeqCst);Ok(())
        }));
        register_session_hooks(api,service.clone());
        let lazy=service.clone();api.register_lazy_tool_activator(Arc::new(move |name|lazy.lock().unwrap_or_else(std::sync::PoisonError::into_inner).activate_tool(name).unwrap_or_else(|error|std::panic::panic_any(error))));
        let adapter=Arc::new(Mutex::new(crate::native_search::AnthropicNativeToolSearchAdapter::default()));
        let request_adapter=adapter.clone();let request_service=service.clone();let mcp_enabled=self.mcp_native_enabled.clone();let actions=self.actions.clone();let runtime=api.runtime.clone();
        api.on(maho_ext_api::EventKind::BeforeProviderRequest,Arc::new(move |event,ctx|{
            let service=request_service.clone();let adapter=request_adapter.clone();let mcp=mcp_enabled.clone();let actions=actions.clone();let runtime=runtime.clone();
            Box::pin(async move {
                let maho_ext_api::ExtensionEvent::BeforeProviderRequest {payload,model,..}=event else{return Ok(maho_ext_api::EventResult::None);};
                let mut service=service.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let catalog=service.get_catalog()?;let active=runtime.session_actions()?.get_active_tools()?;let enabled=mcp();let tools=actions.get_all_tools()?;
                let model=model.as_ref().or(ctx.model.as_ref());let target=model.map(|model|crate::native_support::AnthropicToolSearchTarget::Model {api:&model.api,id:&model.id,provider:&model.provider,supports_tool_references:model.compat.as_ref().and_then(|compat|compat.get("supportsToolReferences")).and_then(serde_json::Value::as_bool)});
                let next=adapter.lock().unwrap_or_else(std::sync::PoisonError::into_inner).apply_before_request(target.as_ref(),payload,native_search_enabled(&catalog,&active,enabled),&crate::native_search::AnthropicNativeInjectionConfig {search_tool_name:Some(crate::tool::TOOL_SEARCH_TOOL_NAME),catalog:&catalog,is_deferrable:&|name|is_deferrable(name,&catalog,&active,enabled),get_tool_definition:&|name|tools.iter().find(|tool|tool.name==name).map(|tool|crate::native_search::NativeToolDefinition {description:Some(tool.description.clone()),parameters:Some(tool.parameters.clone())})});
                Ok(maho_ext_api::EventResult::ProviderPayload(next))
            })
        }));
        api.on(maho_ext_api::EventKind::AfterProviderResponse,Arc::new(move |event,_|{
            let adapter=adapter.clone();let service=service.clone();Box::pin(async move {
                if let maho_ext_api::ExtensionEvent::AfterProviderResponse {status,..}=event&&let Some(reason)=adapter.lock().unwrap_or_else(std::sync::PoisonError::into_inner).note_response_status(*status){service.lock().unwrap_or_else(std::sync::PoisonError::into_inner).note_native_injection_failure(reason.into());}
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}
pub fn register_session_hooks(api:&mut maho_ext_api::ExtensionApi,service:std::sync::Arc<std::sync::Mutex<crate::service::ToolSearchService>>) {
    let start_service=service.clone();
    api.on(maho_ext_api::EventKind::SessionStart,std::sync::Arc::new(move |_event,ctx| {
        let service=start_service.clone();
        Box::pin(async move {
            let mut service=service.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            service.begin_session()?;
            let entries=ctx.session_manager.get_entries().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
            service.maybe_rehydrate_from_history(&entries)?;
            Ok(maho_ext_api::EventResult::None)
        })
    }));
    api.on(maho_ext_api::EventKind::Context,std::sync::Arc::new(move |event,_ctx| {
        let service=service.clone();
        Box::pin(async move {
            if let maho_ext_api::ExtensionEvent::Context{messages}=event {
                let messages=messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>,_>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                service.lock().unwrap_or_else(std::sync::PoisonError::into_inner).maybe_rehydrate_from_history(&messages)?;
            }
            Ok(maho_ext_api::EventResult::None)
        })
    }));
}
pub fn native_search_enabled(catalog:&[ToolSearchDocument],active:&[String],mcp_native_enabled:bool)->bool { catalog.iter().any(|document|match document.source { ToolSearchSource::Extension=>!active.contains(&document.name),ToolSearchSource::Mcp=>mcp_native_enabled }) }
pub fn is_deferrable(name:&str,catalog:&[ToolSearchDocument],active:&[String],mcp_native_enabled:bool)->bool { catalog.iter().find(|document|document.name==name).is_some_and(|document|match document.source { ToolSearchSource::Mcp=>mcp_native_enabled,ToolSearchSource::Extension=>!active.iter().any(|active|active==name) }) }
#[cfg(test)]
mod tests {
    use super::*;
    struct EmptyActions;
    impl maho_ext_api::ExtensionActions for EmptyActions {
        fn send_message(&self,_:maho_ext_api::CustomMessage,_:maho_ext_api::SendMessageOptions)->Result<(),maho_ext_api::ExtensionFailure>{panic!("not used")}
        fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),maho_ext_api::ExtensionFailure>{panic!("not used")}
        fn append_entry(&self,_:&str,_:Option<serde_json::Value>)->Result<(),maho_ext_api::ExtensionFailure>{panic!("not used")}
        fn get_all_tools(&self)->Result<Vec<maho_ext_api::ToolInfo>,maho_ext_api::ExtensionFailure>{Ok(vec![])}
    }
    #[test]
    fn full_extension_registers_lazy_activation_and_provider_events_without_eager_search() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("tool-search",Default::default(),maho_ext_api::SourceInfo {source:"builtin".into(),..Default::default()}),Default::default(),Default::default(),Default::default());
        maho_ext_api::Extension::register(&ToolSearchExtension {actions:std::sync::Arc::new(EmptyActions),mcp_native_enabled:std::sync::Arc::new(||false)},&mut api);
        assert!(api.registered.tools.is_empty());assert_eq!(api.registered.lazy_tool_activators.len(),1);
        for kind in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::Context,maho_ext_api::EventKind::BeforeProviderRequest,maho_ext_api::EventKind::AfterProviderResponse]{assert_eq!(api.registered.handlers[&kind].len(),1);}
    }
    fn document(source:ToolSearchSource)->ToolSearchDocument { ToolSearchDocument{name:"read".into(),label:"Read".into(),aliases:vec![],description:None,search_text:None,keywords:vec![],source,group:"test".into(),owner_label:"test".into(),registration_id:"test".into()} }
    #[test] fn extension_deferral_requires_inactive_tool() { let catalog=[document(ToolSearchSource::Extension)]; assert!(native_search_enabled(&catalog,&[],false)); assert!(!native_search_enabled(&catalog,&["read".into()],true)); assert!(!is_deferrable("missing",&catalog,&[],true)); }
    #[test] fn mcp_deferral_uses_native_setting_not_active_set() { let catalog=[document(ToolSearchSource::Mcp)]; assert!(!is_deferrable("read",&catalog,&[],false)); assert!(is_deferrable("read",&catalog,&["read".into()],true)); }
}
