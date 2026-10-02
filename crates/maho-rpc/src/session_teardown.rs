use crate::session_registry::{RpcSessionState,SessionCloseState,RpcSessionRegistryError};
pub struct CloseClaim{pub finalizer:Option<bool>,pub mark_detached:Option<String>}
pub async fn dispose_runtime(runtime:&maho_core::agent_session_runtime::AgentSessionRuntime,scope:&maho_ai::node::provider_scope::ProviderScope)->Result<(),maho_ai::node::provider_scope::ProviderScopeError>{
    maho_ai::node::provider_scope::run_with_provider_scope_async(scope,async{
        runtime.session().abort().await;
        runtime.session().wait_for_idle().await;
        runtime.dispose().await;
    }).await?;
    scope.close();
    Ok(())
}
pub fn begin_session_close(entry:&mut SessionCloseState,detach:bool)->Result<CloseClaim,RpcSessionRegistryError>{
    if entry.state==RpcSessionState::Closing{return Ok(CloseClaim{finalizer:Some(false),mark_detached:None});}
    if entry.state!=RpcSessionState::Open{return Err(RpcSessionRegistryError::new("unknown_session",None,None));}
    entry.attachments-=1;
    if entry.attachments>0{return Ok(CloseClaim{finalizer:None,mark_detached:None});}
    if detach&&entry.retain_on_disconnect{entry.attachments=0;return Ok(CloseClaim{finalizer:None,mark_detached:if entry.writing{None}else{entry.reservation_key.clone()}});}
    entry.state=RpcSessionState::Closing;Ok(CloseClaim{finalizer:Some(true),mark_detached:None})
}
#[cfg(test)]mod tests{use super::*;fn entry()->SessionCloseState{SessionCloseState{state:RpcSessionState::Open,attachments:1,retain_on_disconnect:true,reservation_key:Some("path".into()),writing:false}}#[test]fn retained_detach_keeps_writing_claim_and_explicit_close_finalizes(){let mut entry=entry();entry.writing=true;assert!(begin_session_close(&mut entry,true).unwrap().mark_detached.is_none());assert_eq!(entry.state,RpcSessionState::Open);assert_eq!(entry.attachments,0);assert_eq!(begin_session_close(&mut entry,false).unwrap().finalizer,Some(true));assert_eq!(begin_session_close(&mut entry,false).unwrap().finalizer,Some(false));}#[test]fn only_last_idle_retained_attachment_marks_detached(){let mut entry=entry();entry.attachments=2;assert!(begin_session_close(&mut entry,true).unwrap().mark_detached.is_none());assert_eq!(begin_session_close(&mut entry,true).unwrap().mark_detached.as_deref(),Some("path"));assert_eq!(entry.state,RpcSessionState::Open);}}
