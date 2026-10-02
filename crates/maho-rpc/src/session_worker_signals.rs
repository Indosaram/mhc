use std::sync::{Arc,Mutex,Condvar};
use crate::session_worker_protocol::{SessionWriteGrant,WorkerDisplay,worker_credit_code};
#[derive(Default)]struct SignalState{code:i32,rendered:bool,width:f64,revision:f64}
#[derive(Clone,Default)]pub struct WorkerSignal(Arc<(Mutex<SignalState>,Condvar)>);
impl WorkerSignal{
    pub fn acknowledge_grant(&self,grant:SessionWriteGrant){let (lock,notify)=&*self.0;lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).code=worker_credit_code(grant);notify.notify_all();}
    pub fn acknowledge(&self,granted:bool){self.acknowledge_grant(if granted{SessionWriteGrant::Granted}else{SessionWriteGrant::Conflict});}
    pub fn respond_display(&self,display:&WorkerDisplay){let (lock,notify)=&*self.0;let mut state=lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.width=display.width;state.revision=display.revision;state.rendered=display.rendered;state.code=worker_credit_code(SessionWriteGrant::Granted);notify.notify_all();}
    pub fn wait(&self,timeout:std::time::Duration)->i32{let (lock,notify)=&*self.0;let state=lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let (state,_)=notify.wait_timeout_while(state,timeout,|state|state.code==0).unwrap_or_else(std::sync::PoisonError::into_inner);state.code}
    pub fn display(&self)->(f64,f64,bool){let state=self.0.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);(state.width,state.revision,state.rendered)}
}
#[cfg(test)]mod tests{use super::*;#[test]fn grant_is_retained_before_wait_and_display_published_before_release(){let signal=WorkerSignal::default();signal.acknowledge_grant(SessionWriteGrant::Limit);assert_eq!(signal.wait(std::time::Duration::ZERO),3);let signal=WorkerSignal::default();signal.respond_display(&WorkerDisplay{width:80.,revision:2.,rendered:true,capabilities:vec![]});assert_eq!(signal.wait(std::time::Duration::ZERO),1);assert_eq!(signal.display(),(80.,2.,true));}#[test]fn blocked_worker_is_released_by_host(){let signal=WorkerSignal::default();let worker=signal.clone();let thread=std::thread::spawn(move||worker.wait(std::time::Duration::from_secs(2)));signal.acknowledge(false);assert_eq!(thread.join().unwrap(),2);}}
