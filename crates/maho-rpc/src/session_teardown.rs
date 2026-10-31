use crate::session_registry::{RpcSessionState,SessionCloseState,RpcSessionRegistryError};
pub struct CloseClaim{pub finalizer:Option<bool>,pub mark_detached:Option<String>}
pub enum RuntimeCloseCompletion{
    Settled(Result<Result<(),maho_ai::node::provider_scope::ProviderScopeError>,tokio::task::JoinError>),
    Draining(tokio::task::JoinHandle<Result<(),maho_ai::node::provider_scope::ProviderScopeError>>),
}
/// Awaits a session-path reservation's removal for at most `close_grace_ms` (senpi `releaseWithinGrace`): a removal
/// that fails or runs late is reported and the close goes on, so a stuck filesystem never pins a session open.
pub async fn release_within_grace<F,E>(close_grace_ms:u64,handle:&str,release:F)
where F:std::future::Future<Output=Result<(),E>>,E:std::fmt::Display{
    release_within_grace_reporting(close_grace_ms,handle,release,|line|eprintln!("{line}")).await;
}
async fn release_within_grace_reporting<F,E>(close_grace_ms:u64,handle:&str,release:F,mut report:impl FnMut(String))
where F:std::future::Future<Output=Result<(),E>>,E:std::fmt::Display{
    let failure=|cause:String|format!("senpi rpc session {handle} teardown failed: {cause}");
    match tokio::time::timeout(std::time::Duration::from_millis(close_grace_ms),release).await{
        Ok(Ok(()))=>{}
        Ok(Err(cause))=>report(failure(format!("Error: {cause}"))),
        Err(_)=>report(failure(format!("Error: session path reservation was not removed within {close_grace_ms} ms; closing anyway"))),
    }
}
pub async fn close_runtime_with_grace(runtime:std::sync::Arc<maho_core::agent_session_runtime::AgentSessionRuntime>,scope:maho_ai::node::provider_scope::ProviderScope,grace_ms:u64)->RuntimeCloseCompletion{
    let mut disposal=tokio::spawn(async move{dispose_runtime(&runtime,&scope).await});
    match tokio::time::timeout(std::time::Duration::from_millis(grace_ms),&mut disposal).await{
        Ok(result)=>RuntimeCloseCompletion::Settled(result),
        Err(_)=>RuntimeCloseCompletion::Draining(disposal),
    }
}
pub fn admit_session_close(writer:&crate::session_event_writer::SessionWriterActor,session_id:&str,response:&serde_json::Value,entry:&mut SessionCloseState,detach:bool)->Result<Option<(u64,CloseClaim)>,String>{
    let terminal=entry.state==RpcSessionState::Open&&entry.attachments<=1&&!(detach&&entry.retain_on_disconnect);
    let Some(id)=writer.reserve_close_response(session_id,response,terminal)?else{return Ok(None);};
    match begin_session_close(entry,detach){
        Ok(claim)=>Ok(Some((id,claim))),
        Err(error)=>{writer.release_close_response(id);Err(error.to_string())},
    }
}
pub async fn dispose_runtime(runtime:&maho_core::agent_session_runtime::AgentSessionRuntime,scope:&maho_ai::node::provider_scope::ProviderScope)->Result<(),maho_ai::node::provider_scope::ProviderScopeError>{
    maho_ai::node::provider_scope::run_with_provider_scope_async(scope,async{
        runtime.session().abort().await;
        runtime.session().wait_for_idle().await;
        runtime.dispose().await;
    }).await?;
    scope.close();
    Ok(())
}
/// Session-owned work that outlives its client: a turn, a tool, anything still appending records (senpi `sessionIsWriting`).
pub fn session_is_writing(worker_busy:Option<bool>,runtime:Option<&maho_core::agent_session_runtime::AgentSessionRuntime>)->bool{
    worker_busy==Some(true)||runtime.is_some_and(|runtime|runtime.session().is_session_busy())
}
pub fn begin_session_close(entry:&mut SessionCloseState,detach:bool)->Result<CloseClaim,RpcSessionRegistryError>{
    if entry.state==RpcSessionState::Closing{return Ok(CloseClaim{finalizer:Some(false),mark_detached:None});}
    if entry.state!=RpcSessionState::Open{return Err(RpcSessionRegistryError::new("unknown_session",None,None));}
    entry.attachments-=1;
    if entry.attachments>0{return Ok(CloseClaim{finalizer:None,mark_detached:None});}
    if detach&&entry.retain_on_disconnect{entry.attachments=0;return Ok(CloseClaim{finalizer:None,mark_detached:if entry.writing{None}else{entry.reservation_key.clone()}});}
    entry.state=RpcSessionState::Closing;Ok(CloseClaim{finalizer:Some(true),mark_detached:None})
}
#[cfg(test)]mod tests{use super::*;fn entry()->SessionCloseState{SessionCloseState{state:RpcSessionState::Open,attachments:1,retain_on_disconnect:true,reservation_key:Some("path".into()),writing:false}}#[test]fn retained_detach_keeps_writing_claim_and_explicit_close_finalizes(){let mut entry=entry();entry.writing=true;assert!(begin_session_close(&mut entry,true).unwrap().mark_detached.is_none());assert_eq!(entry.state,RpcSessionState::Open);assert_eq!(entry.attachments,0);assert_eq!(begin_session_close(&mut entry,false).unwrap().finalizer,Some(true));assert_eq!(begin_session_close(&mut entry,false).unwrap().finalizer,Some(false));}#[test]fn writing_is_a_busy_worker_or_runtime(){assert!(session_is_writing(Some(true),None));assert!(!session_is_writing(Some(false),None));assert!(!session_is_writing(None,None));}#[test]fn only_last_idle_retained_attachment_marks_detached(){let mut entry=entry();entry.attachments=2;assert!(begin_session_close(&mut entry,true).unwrap().mark_detached.is_none());assert_eq!(begin_session_close(&mut entry,true).unwrap().mark_detached.as_deref(),Some("path"));assert_eq!(entry.state,RpcSessionState::Open);}}
#[cfg(test)]
mod grace_tests{
    use super::*;
    #[tokio::test(start_paused=true)] async fn late_and_failed_removals_are_reported_not_awaited(){
        let mut lines=Vec::new();
        release_within_grace_reporting(100,"h1",async{Ok::<(),String>(())},|line|lines.push(line)).await;
        assert!(lines.is_empty());
        release_within_grace_reporting(100,"h1",async{Err::<(),_>("denied")},|line|lines.push(line)).await;
        release_within_grace_reporting(100,"h2",std::future::pending::<Result<(),String>>(),|line|lines.push(line)).await;
        assert_eq!(lines,vec!["senpi rpc session h1 teardown failed: Error: denied".to_owned(),"senpi rpc session h2 teardown failed: Error: session path reservation was not removed within 100 ms; closing anyway".to_owned()]);
    }
}
