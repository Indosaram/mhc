use crate::session_worker_protocol::{WorkerDisplay,WorkerSnapshot};
#[derive(Debug,Clone,PartialEq)]pub enum WorkerControl{Display(WorkerDisplay),CancelUi}
#[derive(Default)]pub struct WorkerControls{display_pending:bool,cancel_pending:bool,latest_display:Option<WorkerDisplay>,stopped:bool}
impl WorkerControls{
    pub fn post(&mut self,control:WorkerControl)->Option<WorkerControl>{
        if self.stopped{return None;}
        match &control{WorkerControl::Display(display)=>{if self.display_pending{self.latest_display=Some(display.clone());return None;}self.display_pending=true;},WorkerControl::CancelUi=>{if self.cancel_pending{return None;}self.cancel_pending=true;}}
        Some(control)
    }
    pub fn control_done(&mut self,display:bool)->Option<WorkerControl>{
        if display{self.display_pending=false;if let Some(latest)=self.latest_display.take(){return self.post(WorkerControl::Display(latest));}}else{self.cancel_pending=false;}None
    }
    pub fn stop(&mut self){self.stopped=true;}
}
pub fn worker_busy(active_requests:usize,snapshot:Option<&WorkerSnapshot>)->bool{active_requests>0||snapshot.is_some_and(|snapshot|snapshot.busy)}
pub fn worker_handoff_busy(active_requests:usize,snapshot:Option<&WorkerSnapshot>)->bool{active_requests>0||snapshot.is_some_and(|snapshot|snapshot.handoff_busy.unwrap_or(snapshot.busy))}
pub fn receive_reservation(message:crate::session_worker_protocol::SessionWorkerToHost,stopped:bool,reserve:impl FnOnce(&str)->crate::session_worker_protocol::SessionWriteGrant)->Option<crate::session_worker_protocol::SessionWorkerToHost>{
    if stopped{match &message{
        crate::session_worker_protocol::SessionWorkerToHost::Reserve{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Snapshot{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Output{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Width{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Capabilities{signal,..}=>signal.acknowledge(false),
        _=>{},
    }return None;}
    match message{crate::session_worker_protocol::SessionWorkerToHost::Reserve{path,signal}=>{signal.acknowledge_grant(reserve(&path));None},message=>Some(message)}
}
pub fn commit_output_activity(snapshot:&mut Option<WorkerSnapshot>,replacement:Option<WorkerSnapshot>,busy:bool,handoff_busy:Option<bool>,streaming:bool){
    if let Some(replacement)=replacement{*snapshot=Some(replacement);}else if let Some(snapshot)=snapshot{snapshot.busy=busy;snapshot.handoff_busy=handoff_busy;snapshot.streaming=streaming;snapshot.state["isStreaming"]=streaming.into();}
}
pub fn receive_snapshot(snapshot:&mut Option<WorkerSnapshot>,replacement:WorkerSnapshot,signal:&crate::session_worker_signals::WorkerSignal,settled:bool,reconcile:impl FnOnce(&[String]),notify_settled:impl FnOnce()){
    *snapshot=Some(replacement);
    reconcile(&snapshot.as_ref().expect("committed snapshot").live_session_paths);
    signal.acknowledge(true);
    if settled{notify_settled();}
}
pub async fn settle_output_credit(signal:&crate::session_worker_signals::WorkerSignal,consumed:impl std::future::Future<Output=Result<(),String>>,fail:impl FnOnce(&str)){
    match consumed.await{Ok(())=>signal.acknowledge(true),Err(error)=>{signal.acknowledge(false);fail(&error);}}
}
#[cfg(test)]mod tests{
    use super::*;fn display(revision:f64)->WorkerControl{WorkerControl::Display(WorkerDisplay{revision,width:80.,rendered:false,capabilities:vec![]})}
    #[test]fn output_activity_updates_identity_before_publication_without_creating_runtime(){let mut snapshot=None;commit_output_activity(&mut snapshot,None,true,Some(true),true);assert!(snapshot.is_none());let replacement=WorkerSnapshot{state:serde_json::json!({"sessionId":"durable","isStreaming":false}),session_path:Some("/session".into()),live_session_paths:vec!["/session".into()],busy:false,handoff_busy:Some(false),streaming:false};commit_output_activity(&mut snapshot,Some(replacement),true,Some(true),true);assert!(!snapshot.as_ref().unwrap().busy);commit_output_activity(&mut snapshot,None,true,Some(true),true);let snapshot=snapshot.unwrap();assert_eq!(snapshot.state["sessionId"],"durable");assert_eq!(snapshot.state["isStreaming"],true);assert!(snapshot.busy);assert_eq!(snapshot.session_path.as_deref(),Some("/session"));}
    #[test]fn control_debt_coalesces_latest_display_and_cancel_independently(){let mut controls=WorkerControls::default();assert_eq!(controls.post(display(1.)),Some(display(1.)));assert!(controls.post(display(2.)).is_none());assert!(controls.post(display(3.)).is_none());assert_eq!(controls.post(WorkerControl::CancelUi),Some(WorkerControl::CancelUi));assert!(controls.post(WorkerControl::CancelUi).is_none());assert_eq!(controls.control_done(true),Some(display(3.)));assert!(controls.post(display(4.)).is_none());controls.control_done(false);assert_eq!(controls.post(WorkerControl::CancelUi),Some(WorkerControl::CancelUi));controls.stop();assert!(controls.control_done(true).is_none());assert!(controls.post(display(5.)).is_none());}
    #[test]fn handoff_uses_specific_busy_field_but_request_debt_always_holds(){let mut snapshot=WorkerSnapshot{state:serde_json::json!({}),session_path:None,live_session_paths:vec![],busy:true,handoff_busy:Some(false),streaming:false};assert!(worker_busy(0,Some(&snapshot)));assert!(!worker_handoff_busy(0,Some(&snapshot)));assert!(worker_handoff_busy(1,Some(&snapshot)));snapshot.handoff_busy=None;assert!(worker_handoff_busy(0,Some(&snapshot)));assert!(!worker_busy(0,None));}
}
