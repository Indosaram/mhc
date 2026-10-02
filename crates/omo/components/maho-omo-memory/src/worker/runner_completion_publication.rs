use std::path::Path;
use memory_core::{identity::resolve::MemoryIdentity,reflection::{ReservedRun,ReflectionTrigger}};
use super::{runner_types::{ExecutionResult,ReflectionRunResult,ReflectionReservationPort},resolve_model::ReflectionModelResolution,completion::{ReflectionCompletionRecord,CompletionDelivery,DeliveryStatus,ReflectionLiveSession,record_reflection_completion},health::read_reflection_health};
pub struct SettleReflectionRunInput<'a> {
    pub run:&'a ReservedRun,pub result:ExecutionResult,pub started_at:&'a str,pub resolution:&'a ReflectionModelResolution,pub identity:&'a MemoryIdentity,pub reservation:&'a dyn ReflectionReservationPort,pub now_ms:i64,pub suppress_completion_notification:bool,
}
pub fn settle_reflection_run(input:SettleReflectionRunInput<'_>,live:Option<&mut dyn ReflectionLiveSession>,ensure_renderer:impl FnOnce(),health_alert:impl FnOnce(&Path))->Result<ReflectionRunResult,String> {
    let outcome=serde_json::from_value(input.result.outcome.clone().into()).map_err(|error|error.to_string())?;
    let transition=input.reservation.complete(&input.run.run_id,outcome)?;
    ensure_renderer();
    let finished=chrono::DateTime::from_timestamp_millis(input.now_ms).ok_or("Invalid completion timestamp")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let dir=input.identity.paths.reflection.join("completions");
    let health=read_reflection_health(&dir,crate::status::MEMORY_HEALTH_SCAN_LIMIT,input.now_ms);
    let (category,model,thinking)=match input.resolution {
        ReflectionModelResolution::Resolved{category,model,thinking,..}=> {
            let chosen=input.result.model.clone().unwrap_or_else(||model.clone());
            let thinking=if input.result.model.is_none(){thinking.clone()}else{input.result.thinking.clone()};
            (category.clone(),Some(chosen),thinking)
        },
        ReflectionModelResolution::CategoryUnavailable{category,..}=>(category.clone(),None,None),
    };
    let duration=chrono::DateTime::parse_from_rfc3339(input.started_at).map_err(|error|error.to_string()).map(|start|(input.now_ms-start.timestamp_millis()).max(0) as f64)?;
    let record=ReflectionCompletionRecord {schema_version:1,run_id:input.run.run_id.clone(),identity:input.identity.id.clone(),category,model,thinking,conversation_ids:input.run.request.conversation_ids.clone(),trigger:input.run.request.trigger,origin:if input.run.request.trigger==ReflectionTrigger::Dream{input.run.request.origin}else{None},outcome:input.result.outcome.clone(),reason:input.result.reason.clone(),detail:input.result.detail.clone(),started_at:input.started_at.into(),finished_at:finished,duration_ms:Some(duration),merged_commit_sha:None,files_changed:None,consecutive_failures:Some(if input.result.outcome=="failed"{health.streak+1}else{0}),delivery:CompletionDelivery{status:DeliveryStatus::Pending,session_id:None,consumed_at:None}};
    let completion=if input.suppress_completion_notification {
        let durable=super::completion::ensure_reflection_completion(&dir,&record).map_err(|error|error.to_string())?;
        match live {
            Some(live) if durable.delivery.status!=DeliveryStatus::Consumed=>super::completion::deliver_reflection_completion(&dir,&durable,live,false,input.now_ms).map_err(|error|error.to_string())?,
            _=>durable,
        }
    }else{record_reflection_completion(&dir,&record,live,input.now_ms).map_err(|error|error.to_string())?};
    health_alert(&dir);
    Ok(ReflectionRunResult{run_id:input.run.run_id.clone(),outcome:input.result.outcome,reason:input.result.reason,detail:input.result.detail,completion,launch:transition.launch})
}
pub fn publish_finalized_reflection_run(mut result:ReflectionRunResult,identity:&MemoryIdentity,now_ms:i64,live:Option<&mut dyn ReflectionLiveSession>,ensure_renderer:impl FnOnce(),merged_metadata:impl FnOnce(&str)->Result<(Option<String>,Option<usize>),String>,health_alert:impl FnOnce(&Path))->Result<ReflectionRunResult,String> {
    ensure_renderer();let dir=identity.paths.reflection.join("completions");let health=read_reflection_health(&dir,crate::status::MEMORY_HEALTH_SCAN_LIMIT,now_ms);
    let started=chrono::DateTime::parse_from_rfc3339(&result.completion.started_at).map_err(|error|error.to_string())?;
    let finished=chrono::DateTime::parse_from_rfc3339(&result.completion.finished_at).map_err(|error|error.to_string())?;
    result.completion.duration_ms=Some((finished.timestamp_millis()-started.timestamp_millis()).max(0) as f64);
    if result.completion.outcome=="merged" {
        let (sha,files)=merged_metadata(&result.completion.run_id)?;
        if sha.is_some(){result.completion.merged_commit_sha=sha;}
        if files.is_some(){result.completion.files_changed=files;}
    }
    result.completion.consecutive_failures=Some(if result.completion.outcome=="failed"{health.streak}else{0});
    result.completion=record_reflection_completion(&dir,&result.completion,live,now_ms).map_err(|error|error.to_string())?;health_alert(&dir);Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Live{entries:usize,notifications:usize,completions:usize}
    impl ReflectionLiveSession for Live {
        fn session_id(&self)->&str{"session"}
        fn append_entry(&mut self,_:&str,_:serde_json::Value){self.entries+=1;}
        fn notify(&mut self,_:&str,_:bool)->Result<(),String>{self.notifications+=1;Ok(())}
        fn warn(&mut self,_:&str,_:&str){panic!("unexpected warning")}
        fn on_completion(&mut self,_:&str){self.completions+=1;}
    }
    #[test]
    fn unavailable_category_suppresses_notification_and_completion_callback() {
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        let run=ReservedRun{run_id:"run".into(),request:memory_core::reflection::ReflectionRequest{trigger:ReflectionTrigger::Manual,origin:None,conversation_ids:vec!["one".into()],snapshots:vec![],focus:None,recent_n:None,target_doc:None},reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None};
        let resolution=ReflectionModelResolution::CategoryUnavailable{category:"quick".into(),cause:"no_registry",attempted_chain:None,missing_providers:None};let reservation=Reservation(std::cell::Cell::new(0));let mut live=Live::default();
        let result=settle_reflection_run(SettleReflectionRunInput{run:&run,result:ExecutionResult{outcome:"failed".into(),reason:Some("category_unavailable".into()),detail:None,model:None,thinking:None},started_at:"1970-01-01T00:00:00.000Z",resolution:&resolution,identity:&identity,reservation:&reservation,now_ms:1000,suppress_completion_notification:true},Some(&mut live),||{},|_|{}).unwrap();
        assert_eq!(live.entries,1);assert_eq!(live.notifications,0);assert_eq!(live.completions,0);assert_eq!(result.completion.delivery.status,DeliveryStatus::Consumed);assert!(result.completion.model.is_none());
    }
    struct Reservation(std::cell::Cell<usize>);
    impl ReflectionReservationPort for Reservation {
        fn read_state(&self)->Result<memory_core::reflection::ReservationState,String>{panic!("settle completes directly")}
        fn complete(&self,id:&str,outcome:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{assert_eq!(id,"run");self.0.set(self.0.get()+1);Ok(memory_core::reflection::CompletionResult{outcome,launch:None})}
    }
    #[test]
    fn resolved_execution_model_does_not_inherit_stale_thinking() {
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        let run=ReservedRun{run_id:"run".into(),request:memory_core::reflection::ReflectionRequest{trigger:ReflectionTrigger::Manual,origin:None,conversation_ids:vec!["one".into()],snapshots:vec![],focus:None,recent_n:None,target_doc:None},reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None};
        let resolution=ReflectionModelResolution::Resolved{category:"quick".into(),model:"old/model".into(),thinking:Some("high".into()),source:None,fallbacks:vec![]};
        let reservation=Reservation(std::cell::Cell::new(0));let ensured=std::cell::Cell::new(false);
        let result=settle_reflection_run(SettleReflectionRunInput{run:&run,result:ExecutionResult{outcome:"failed".into(),reason:Some("spawn_failed".into()),detail:None,model:Some("new/model".into()),thinking:None},started_at:"1970-01-01T00:00:00.000Z",resolution:&resolution,identity:&identity,reservation:&reservation,now_ms:1000,suppress_completion_notification:false},None,||{assert_eq!(reservation.0.get(),1);ensured.set(true);},|dir|{assert!(ensured.get());assert!(dir.join("run.json").exists());}).unwrap();
        assert_eq!(result.completion.model.as_deref(),Some("new/model"));assert!(result.completion.thinking.is_none());assert_eq!(result.completion.duration_ms,Some(1000.0));assert_eq!(result.completion.consecutive_failures,Some(1));assert_eq!(result.completion.delivery.status,DeliveryStatus::Pending);
    }
}
