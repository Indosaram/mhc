use std::{future::Future, sync::{Arc, Mutex}, time::Duration};
use maho_ai::utils::abort::{AbortReason, AbortSignal, ListenerId};
use tokio::sync::watch;
use crate::timeouts::idle_timeout::{IdleTimeout, IdleTimeoutOptions, TimeoutPauseHandle};
use super::types::{EvalKernel, KernelInterruptHandle};

pub struct CellIdleWatchdogOptions {
    pub timeout_ms: u64,
    pub max_pause_grace_ms: u64,
    pub on_timeout: Arc<dyn Fn(String) + Send + Sync>,
}
struct State {
    active: bool,
    kernel: Option<Arc<dyn EvalKernel>>,
    watchdog: Option<IdleTimeout>,
    watchdog_task: Option<tokio::task::JoinHandle<()>>,
    listener: Option<ListenerId>,
}
pub struct CellExecution {
    cell_id: String,
    caller: AbortSignal,
    on_abort: Arc<dyn Fn(AbortReason) + Send + Sync>,
    state: Mutex<State>,
    aborted: watch::Sender<Option<AbortReason>>,
    detached: watch::Sender<bool>,
    interrupt: Mutex<Option<KernelInterruptHandle>>,
}
impl CellExecution {
    pub fn new(caller: AbortSignal, cell_id: String, idle: Option<CellIdleWatchdogOptions>, on_abort: Arc<dyn Fn(AbortReason) + Send + Sync>) -> Arc<Self> {
        let (aborted,_)=watch::channel(None);
        let (detached,_)=watch::channel(false);
        let execution=Arc::new(Self {cell_id,caller:caller.clone(),on_abort,state:Mutex::new(State {active:true,kernel:None,watchdog:None,watchdog_task:None,listener:None}),aborted,detached,interrupt:Mutex::new(None)});
        if let Some(idle)=idle {execution.rearm_idle(idle.timeout_ms,idle.max_pause_grace_ms,idle.on_timeout);}
        let weak=Arc::downgrade(&execution);
        let listener=caller.add_abort_listener(move |reason|{if let Some(execution)=weak.upgrade() {execution.cancel(reason.clone());}});
        execution.state.lock().expect("execution lock").listener=Some(listener);
        execution
    }
    pub fn rearm_idle(&self, timeout_ms:u64, max_pause_grace_ms:u64, on_timeout:Arc<dyn Fn(String)+Send+Sync>) {
        let mut state=self.state.lock().expect("execution lock");
        if let Some(task)=state.watchdog_task.take() {task.abort();}
        if let Some(watchdog)=state.watchdog.take() {watchdog.dispose();}
        let watchdog=IdleTimeout::new(IdleTimeoutOptions {cell_id:self.cell_id.clone(),timeout_ms,max_pause_grace_ms:Some(max_pause_grace_ms),deadline:Some(tokio::time::Instant::now()+Duration::from_millis(max_pause_grace_ms))});
        let mut signal=watchdog.signal();
        state.watchdog_task=Some(tokio::spawn(async move {
            if signal.changed().await.is_ok() && let Some(event)=signal.borrow().clone() {on_timeout(event.error);}
        }));
        state.watchdog=Some(watchdog);
    }
    pub fn pause(&self) {if let Some(watchdog)=&self.state.lock().expect("execution lock").watchdog {watchdog.pause();}}
    pub fn resume(&self) {if let Some(watchdog)=&self.state.lock().expect("execution lock").watchdog {watchdog.resume();}}
    pub fn set_kernel(&self, kernel:Arc<dyn EvalKernel>) {self.state.lock().expect("execution lock").kernel=Some(kernel);}
    pub fn detached(&self) -> watch::Receiver<bool> {self.detached.subscribe()}
    pub fn detach(&self) {
        let mut state=self.state.lock().expect("execution lock");
        if !state.active {return;}
        if let Some(watchdog)=state.watchdog.take() {watchdog.dispose();}
        if let Some(task)=state.watchdog_task.take() {task.abort();}
        self.detached.send_replace(true);
    }
    pub fn cancel(self:&Arc<Self>, reason:AbortReason) {
        let kernel={
            let mut state=self.state.lock().expect("execution lock");
            if !state.active {return;}
            state.active=false;
            self.cleanup(&mut state);
            state.kernel.clone()
        };
        (self.on_abort)(reason.clone());
        let Some(kernel)=kernel else {self.aborted.send_replace(Some(reason));return;};
        let execution=Arc::clone(self);
        tokio::spawn(async move {
            let deadline=tokio::time::sleep(Duration::from_millis(100));
            tokio::pin!(deadline);
            let operation=kernel.interrupt(&reason.message,Some(&execution.cell_id));
            tokio::pin!(operation);
            let outcome=tokio::select! {
                result=&mut operation=>Some(result),
                ()=&mut deadline=>{execution.aborted.send_replace(Some(reason.clone()));None}
            };
            let outcome=match outcome {Some(outcome)=>outcome,None=>operation.await};
            match outcome {
                Ok(handle)=>{*execution.interrupt.lock().expect("interrupt lock")=Some(handle);execution.aborted.send_if_modified(|current|{if current.is_some(){false}else{*current=Some(reason.clone());true}});}
                Err(error)=>{execution.aborted.send_if_modified(|current|{if current.is_some(){false}else{*current=Some(AbortReason::new("Error",error));true}});}
            }
        });
    }
    pub fn take_interrupt_handle(&self) -> Option<KernelInterruptHandle> {self.interrupt.lock().expect("interrupt lock").take()}
    pub fn finish(&self) {let mut state=self.state.lock().expect("execution lock");state.active=false;self.cleanup(&mut state);}
    fn cleanup(&self,state:&mut State) {
        if let Some(listener)=state.listener.take() {self.caller.remove_abort_listener(listener);}
        if let Some(watchdog)=state.watchdog.take() {watchdog.dispose();}
        if let Some(task)=state.watchdog_task.take() {task.abort();}
    }
    pub async fn wait<T>(&self, operation:impl Future<Output=Result<T,String>>) -> Result<T,String> {
        let mut aborted=self.aborted.subscribe();
        let cancellation=async {
            loop {
                if let Some(reason)=aborted.borrow().clone() {return reason.message;}
                if aborted.changed().await.is_err() {return "Eval interrupted".into();}
            }
        };
        tokio::pin!(cancellation);
        tokio::select! {
            result=operation=>{
                let active=self.state.lock().expect("execution lock").active;
                if active {result} else {Err(cancellation.await)}
            },
            reason=&mut cancellation=>Err(reason)
        }
    }
}
impl Drop for CellExecution {fn drop(&mut self) {let mut state=self.state.lock().expect("execution lock");self.cleanup(&mut state);}}
