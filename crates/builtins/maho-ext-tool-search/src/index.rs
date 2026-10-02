use crate::engine::document::{ToolSearchDocument,ToolSearchSource};
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
    fn document(source:ToolSearchSource)->ToolSearchDocument { ToolSearchDocument{name:"read".into(),label:"Read".into(),aliases:vec![],description:None,search_text:None,keywords:vec![],source,group:"test".into(),owner_label:"test".into(),registration_id:"test".into()} }
    #[test] fn extension_deferral_requires_inactive_tool() { let catalog=[document(ToolSearchSource::Extension)]; assert!(native_search_enabled(&catalog,&[],false)); assert!(!native_search_enabled(&catalog,&["read".into()],true)); assert!(!is_deferrable("missing",&catalog,&[],true)); }
    #[test] fn mcp_deferral_uses_native_setting_not_active_set() { let catalog=[document(ToolSearchSource::Mcp)]; assert!(!is_deferrable("read",&catalog,&[],false)); assert!(is_deferrable("read",&catalog,&["read".into()],true)); }
}
