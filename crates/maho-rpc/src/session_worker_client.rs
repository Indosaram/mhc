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
#[cfg(test)]mod tests{
    use super::*;fn display(revision:f64)->WorkerControl{WorkerControl::Display(WorkerDisplay{revision,width:80.,rendered:false,capabilities:vec![]})}
    #[test]fn control_debt_coalesces_latest_display_and_cancel_independently(){let mut controls=WorkerControls::default();assert_eq!(controls.post(display(1.)),Some(display(1.)));assert!(controls.post(display(2.)).is_none());assert!(controls.post(display(3.)).is_none());assert_eq!(controls.post(WorkerControl::CancelUi),Some(WorkerControl::CancelUi));assert!(controls.post(WorkerControl::CancelUi).is_none());assert_eq!(controls.control_done(true),Some(display(3.)));assert!(controls.post(display(4.)).is_none());controls.control_done(false);assert_eq!(controls.post(WorkerControl::CancelUi),Some(WorkerControl::CancelUi));controls.stop();assert!(controls.control_done(true).is_none());assert!(controls.post(display(5.)).is_none());}
    #[test]fn handoff_uses_specific_busy_field_but_request_debt_always_holds(){let mut snapshot=WorkerSnapshot{state:serde_json::json!({}),session_path:None,live_session_paths:vec![],busy:true,handoff_busy:Some(false),streaming:false};assert!(worker_busy(0,Some(&snapshot)));assert!(!worker_handoff_busy(0,Some(&snapshot)));assert!(worker_handoff_busy(1,Some(&snapshot)));snapshot.handoff_busy=None;assert!(worker_handoff_busy(0,Some(&snapshot)));assert!(!worker_busy(0,None));}
}
