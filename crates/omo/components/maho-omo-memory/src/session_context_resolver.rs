pub trait MemorySessionManager {
    fn get_usage_totals(&self)->serde_json::Value;
    fn get_session_file(&self)->serde_json::Value;
}
pub trait MemorySessionContext {
    fn get_context_usage(&self)->serde_json::Value;
    fn session_manager(&self)->Option<&dyn MemorySessionManager>;
}
pub fn resolve_parent_context_tokens(context:Option<&dyn MemorySessionContext>)->Option<f64>{
    context?.get_context_usage().as_object()?.get("tokens")?.as_f64().filter(|tokens|*tokens>0.0)
}
pub fn resolve_parent_cache_reusable(context:Option<&dyn MemorySessionContext>)->bool{
    context.and_then(MemorySessionContext::session_manager).and_then(|manager|manager.get_usage_totals().as_object()?.get("cacheRead")?.as_f64()).is_some_and(|read|read>0.0)
}
pub fn resolve_parent_session_file(context:Option<&dyn MemorySessionContext>)->Option<String>{
    context?.session_manager()?.get_session_file().as_str().filter(|file|!file.is_empty()).map(str::to_owned)
}
pub fn resolve_native_parent_context_tokens(context:&maho_ext_api::ExtensionContext)->Result<Option<f64>,maho_ext_api::ExtensionFailure>{
    Ok(context.get_context_usage()?.and_then(|usage|usage.tokens).map(|tokens|tokens as f64).filter(|tokens|*tokens>0.0))
}
pub fn resolve_native_parent_session_file(context:&maho_ext_api::ExtensionContext)->Option<String>{
    context.session_manager.session_file().filter(|path|!path.as_os_str().is_empty()).map(|path|path.to_string_lossy().into_owned())
}
#[cfg(test)]
mod tests{
    use super::*;
    struct Context{usage:serde_json::Value,totals:serde_json::Value,file:serde_json::Value,manager:bool}
    impl MemorySessionContext for Context{fn get_context_usage(&self)->serde_json::Value{self.usage.clone()}fn session_manager(&self)->Option<&dyn MemorySessionManager>{self.manager.then_some(self)}}
    impl MemorySessionManager for Context{fn get_usage_totals(&self)->serde_json::Value{self.totals.clone()}fn get_session_file(&self)->serde_json::Value{self.file.clone()}}
    #[test]fn positive_usage_and_cache_and_nonempty_file(){let ctx=Context{usage:serde_json::json!({"tokens":123.5}),totals:serde_json::json!({"cacheRead":1}),file:serde_json::json!("session.jsonl"),manager:true};assert_eq!(resolve_parent_context_tokens(Some(&ctx)),Some(123.5));assert!(resolve_parent_cache_reusable(Some(&ctx)));assert_eq!(resolve_parent_session_file(Some(&ctx)),Some("session.jsonl".into()));}
    #[test]fn absent_and_malformed_fail_closed(){assert_eq!(resolve_parent_context_tokens(None),None);assert!(!resolve_parent_cache_reusable(None));assert_eq!(resolve_parent_session_file(None),None);for value in [serde_json::Value::Null,serde_json::json!([]),serde_json::json!({}),serde_json::json!({"tokens":0,"cacheRead":0}),serde_json::json!({"tokens":-1,"cacheRead":-1}),serde_json::json!({"tokens":"1","cacheRead":"1"})]{let ctx=Context{usage:value.clone(),totals:value,file:serde_json::json!(""),manager:true};assert_eq!(resolve_parent_context_tokens(Some(&ctx)),None);assert!(!resolve_parent_cache_reusable(Some(&ctx)));assert_eq!(resolve_parent_session_file(Some(&ctx)),None);}let ctx=Context{usage:serde_json::Value::Null,totals:serde_json::json!({"cacheRead":10}),file:serde_json::json!("file"),manager:false};assert!(!resolve_parent_cache_reusable(Some(&ctx)));assert_eq!(resolve_parent_session_file(Some(&ctx)),None);}
}
