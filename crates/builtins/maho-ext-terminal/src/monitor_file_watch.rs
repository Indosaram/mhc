use std::sync::{Arc,Mutex};
use tokio::task::JoinHandle;

pub const FILE_MONITOR_POLL_MS:u64=250;

struct State {paused:bool,stopped:bool,check:Box<dyn FnMut()+Send>}

/// Pausing suppresses filesystem work; resuming checks immediately for changes made while paused.
pub struct FileWatchLoop {state:Arc<Mutex<State>>,task:Option<JoinHandle<()>>}
impl FileWatchLoop {
    pub fn new(check:impl FnMut()+Send+'static)->Self {
        let state=Arc::new(Mutex::new(State {paused:false,stopped:false,check:Box::new(check)}));
        let mut result=Self {state,task:None};result.start();result
    }
    fn start(&mut self) {
        let state=self.state.clone();
        self.task=Some(tokio::spawn(async move {
            let period=std::time::Duration::from_millis(FILE_MONITOR_POLL_MS);
            let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let mut state=state.lock().expect("file watch state");
                if state.stopped {return;}
                if !state.paused {(state.check)();}
            }
        }));
    }
    pub fn pause(&mut self) {
        self.state.lock().expect("file watch state").paused=true;
        if let Some(task)=self.task.take() {task.abort();}
    }
    pub fn resume(&mut self) {
        {let mut state=self.state.lock().expect("file watch state");if state.stopped||!state.paused {return;}state.paused=false;(state.check)();}
        self.start();
    }
    pub fn stop(&mut self) {self.state.lock().expect("file watch state").stopped=true;self.pause();}
}
impl Drop for FileWatchLoop {fn drop(&mut self) {self.stop();}}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    #[tokio::test]
    async fn resume_checks_once_and_stopped_loop_never_restarts() {
        let calls=Arc::new(AtomicUsize::new(0));let count=calls.clone();
        let mut watch=FileWatchLoop::new(move ||{count.fetch_add(1,Ordering::SeqCst);});
        assert_eq!(calls.load(Ordering::SeqCst),0);
        watch.pause();assert!(watch.task.is_none());
        watch.resume();assert_eq!(calls.load(Ordering::SeqCst),1);
        watch.resume();assert_eq!(calls.load(Ordering::SeqCst),1);
        watch.stop();watch.resume();assert!(watch.task.is_none());assert_eq!(calls.load(Ordering::SeqCst),1);
    }
}
