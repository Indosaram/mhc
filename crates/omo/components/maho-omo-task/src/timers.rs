use std::{collections::BTreeMap,sync::{Arc,Mutex,PoisonError,mpsc::{self,Sender,RecvTimeoutError}},time::Duration};
use crate::status_ui::StatusUiTimers;
#[derive(Default)] struct State { next:u64,cancellations:BTreeMap<u64,Sender<()>> }
#[derive(Default)] pub struct HostTimers { state:Arc<Mutex<State>> }
impl StatusUiTimers for HostTimers {
    fn set(&self,callback:Box<dyn FnOnce()+Send>,milliseconds:u64)->u64 {
        let (cancel,receiver)=mpsc::channel();
        let handle={ let mut state=self.state.lock().unwrap_or_else(PoisonError::into_inner); state.next+=1; let handle=state.next; state.cancellations.insert(handle,cancel); handle };
        let weak=Arc::downgrade(&self.state);
        std::thread::spawn(move || {
            if receiver.recv_timeout(Duration::from_millis(milliseconds))==Err(RecvTimeoutError::Timeout) && let Some(state)=weak.upgrade() {
                let active=state.lock().unwrap_or_else(PoisonError::into_inner).cancellations.remove(&handle).is_some();
                if active { callback(); }
            }
        });
        handle
    }
    fn clear(&self,handle:u64) { self.state.lock().unwrap_or_else(PoisonError::into_inner).cancellations.remove(&handle); }
}
impl Drop for HostTimers { fn drop(&mut self) { self.state.lock().unwrap_or_else(PoisonError::into_inner).cancellations.clear(); } }
