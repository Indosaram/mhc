use std::sync::{Arc,Mutex};
use maho_ext_api::{EventKind,EventResult,ExtensionApi,ExtensionContext,ExtensionEvent,ExtensionFailure,ToolResultEvent};
use crate::{context::MemoryIdentityContext,status::MemoryStatusResult,tool_metadata::{MEMORY_TOOL_NAME,MEMORY_APPLY_PATCH_TOOL_NAME}};

pub struct MemoryWriteSession { pub context:MemoryIdentityContext,pub memory_status_attempted:bool }
pub type ResolveMemoryWriteSession=Arc<dyn Fn(&str)->Option<Arc<Mutex<MemoryWriteSession>>>+Send+Sync>;
pub type MemoryWriteRefresh=Arc<dyn Fn(&MemoryIdentityContext,&ExtensionContext)->Result<MemoryStatusResult,String>+Send+Sync>;
pub type MemoryWriteNotification=Arc<dyn Fn(&str)->Result<(),String>+Send+Sync>;
#[derive(Clone)]
pub struct MemoryWriteOptions {
    pub resolve_session:ResolveMemoryWriteSession,
    pub on_memory_write:MemoryWriteNotification,
    pub refresh_status:MemoryWriteRefresh,
}
fn matches_tool_name(name:&str,expected:&str)->bool {
    let name=name.trim().to_lowercase().replace('-',"_");
    let expected=expected.trim().to_lowercase().replace('-',"_");
    name==expected || name.ends_with(&format!("_{expected}"))
}
pub fn is_memory_tool_result(event:&ToolResultEvent)->bool {
    !event.is_error && (matches_tool_name(&event.tool_name,MEMORY_TOOL_NAME) || matches_tool_name(&event.tool_name,MEMORY_APPLY_PATCH_TOOL_NAME))
}
pub fn handle_memory_write(options:&MemoryWriteOptions,event:&ToolResultEvent,context:&ExtensionContext)->Result<(),String> {
    if !is_memory_tool_result(event) { return Ok(()); }
    let Some(id)=crate::wiring_context::session_id_from(Some(context)) else { return Ok(()); };
    let Some(session)=(options.resolve_session)(id) else { return Ok(()); };
    (options.on_memory_write)(id)?;
    refresh_after_write(&session,|identity|(options.refresh_status)(identity,context))
}
fn refresh_after_write(session:&Mutex<MemoryWriteSession>,refresh:impl FnOnce(&MemoryIdentityContext)->Result<MemoryStatusResult,String>)->Result<(),String> {
    let identity={
        let mut state=session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.memory_status_attempted { return Ok(()); }
        state.memory_status_attempted=true;
        state.context.clone()
    };
    let result=refresh(&identity);
    let mut state=session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match result {
        Ok(result)=> { state.memory_status_attempted=result.footer_shown; Ok(()) },
        Err(error)=> { state.memory_status_attempted=false; Err(error) },
    }
}
pub fn register_memory_write_listener(api:&mut ExtensionApi,options:MemoryWriteOptions) {
    let options=Arc::new(options);
    api.on(EventKind::ToolResult,Arc::new(move |event,context| {
        let options=Arc::clone(&options);
        Box::pin(async move {
            if let ExtensionEvent::ToolResult(event)=event { handle_memory_write(&options,event,context).map_err(ExtensionFailure::new)?; }
            Ok(EventResult::None)
        })
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn successful_native_and_prefixed_names_match_after_normalization() {
        for name in ["memory","MEMORY-APPLY-PATCH"," mcp_omo-memory_memory ","custom_memory_apply_patch"] {assert!(matches_tool_name(name,MEMORY_TOOL_NAME)||matches_tool_name(name,MEMORY_APPLY_PATCH_TOOL_NAME));}
        for name in ["read","notmemory","memory_other"] {assert!(!matches_tool_name(name,MEMORY_TOOL_NAME));}
    }
    #[test] fn failed_result_never_triggers_write_refresh() {
        let mut event=ToolResultEvent {tool_call_id:"call".into(),tool_name:"memory".into(),input:serde_json::Value::Null,content:vec![],details:None,is_error:true,usage:None};
        assert!(!is_memory_tool_result(&event)); event.is_error=false; assert!(is_memory_tool_result(&event));
    }
    fn session(root:&std::path::Path)->Mutex<MemoryWriteSession> {
        Mutex::new(MemoryWriteSession { context:MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root,"agent"),crate::binding::MemorySessionBinding {identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0}),memory_status_attempted:false })
    }
    #[test] fn shown_footer_latches_once_and_callback_can_read_session() {
        let root=tempfile::tempdir().unwrap(); let session=session(root.path()); let mut refreshes=0;
        for _ in 0..2 {refresh_after_write(&session,|_| {refreshes+=1; assert!(session.lock().unwrap().memory_status_attempted); Ok(MemoryStatusResult {notified:false,footer_shown:true})}).unwrap();}
        assert_eq!(refreshes,1);
    }
    #[test] fn absent_footer_and_refresh_error_allow_later_retry() {
        let root=tempfile::tempdir().unwrap(); let session=session(root.path());
        refresh_after_write(&session,|_|Ok(MemoryStatusResult {notified:false,footer_shown:false})).unwrap(); assert!(!session.lock().unwrap().memory_status_attempted);
        assert_eq!(refresh_after_write(&session,|_|Err("refresh failed".into())),Err("refresh failed".into())); assert!(!session.lock().unwrap().memory_status_attempted);
        refresh_after_write(&session,|_|Ok(MemoryStatusResult {notified:false,footer_shown:true})).unwrap(); assert!(session.lock().unwrap().memory_status_attempted);
    }
}
