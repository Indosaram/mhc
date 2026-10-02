use crate::{elapsed_ticker::GoalElapsedClock,wait_progress::{GoalWaitKind,GoalWaitLabelInput,ResumptionChannelCounts,format_goal_wait_label}};
use maho_ext_api::{ExtensionContext,ExtensionFailure};
use std::sync::{Arc,Mutex};
pub const GOAL_WAIT_TICK_INTERVAL_MS:u64=1000;
pub const GOAL_WAIT_STATUS_KEY:&str="goal-wait";
pub type GoalWaitRender=Arc<dyn Fn(&ExtensionContext,Option<&str>)->Result<(),ExtensionFailure>+Send+Sync>;
struct WaitState { ctx:Option<ExtensionContext>,kind:Option<GoalWaitKind>,due_at:f64,total_ms:f64,counts:ResumptionChannelCounts,last_status:Option<String> }
impl WaitState {
    fn clear(&mut self) { self.ctx=None; self.kind=None; self.last_status=None; }
    fn tick(&mut self,render:&GoalWaitRender,now:f64)->Result<(),ExtensionFailure> {
        let (Some(ctx),Some(kind))=(&self.ctx,self.kind) else { return Ok(()); };
        let status=ctx.is_idle().then(||format_goal_wait_label(&GoalWaitLabelInput { kind,remaining_ms:(self.due_at-now).max(0.0),total_ms:self.total_ms,channel_counts:self.counts.clone() }));
        if status==self.last_status { return Ok(()); }
        self.last_status=status;
        if let Err(error)=render(ctx,self.last_status.as_deref()) {
            if crate::stale_context::is_stale_extension_context_error(&error) { self.clear(); } else { return Err(error); }
        }
        Ok(())
    }
}
pub struct GoalWaitTicker { state:Arc<Mutex<WaitState>>,render:GoalWaitRender,now:GoalElapsedClock,timer:Option<tokio::task::JoinHandle<Result<(),ExtensionFailure>>> }
impl GoalWaitTicker {
    pub fn new(render:GoalWaitRender,now:GoalElapsedClock)->Self {
        Self { state:Arc::new(Mutex::new(WaitState { ctx:None,kind:None,due_at:0.0,total_ms:0.0,counts:ResumptionChannelCounts::new(),last_status:None })),render,now,timer:None }
    }
    pub fn running(&self)->bool { self.timer.as_ref().is_some_and(|timer|!timer.is_finished()) }
    pub async fn sync(&mut self,ctx:ExtensionContext,input:GoalWaitLabelInput)->Result<(),ExtensionFailure> {
        if self.timer.as_ref().is_some_and(tokio::task::JoinHandle::is_finished) && let Some(timer)=self.timer.take() { timer.await.map_err(|error|ExtensionFailure::new(error.to_string()))??; }
        {
            let mut state=self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
            state.ctx=Some(ctx); state.kind=Some(input.kind); state.due_at=(self.now)()+input.remaining_ms.max(0.0); state.total_ms=input.total_ms; state.counts=input.channel_counts; state.last_status=None;
            state.tick(&self.render,(self.now)())?;
            if state.ctx.is_none() { return Ok(()); }
        }
        if self.timer.is_some() { return Ok(()); }
        let state=Arc::clone(&self.state); let render=Arc::clone(&self.render); let now=Arc::clone(&self.now);
        self.timer=Some(tokio::spawn(async move {
            let duration=std::time::Duration::from_millis(GOAL_WAIT_TICK_INTERVAL_MS);
            let mut interval=tokio::time::interval_at(tokio::time::Instant::now()+duration,duration);
            loop {
                interval.tick().await;
                let mut state=state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                state.tick(&render,now())?;
                if state.ctx.is_none() { return Ok(()); }
            }
        }));
        Ok(())
    }
    pub fn set_channel_counts(&mut self,counts:ResumptionChannelCounts)->Result<(),ExtensionFailure> {
        let mut state=self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
        if state.ctx.is_none() || state.kind!=Some(GoalWaitKind::Monitor) { return Ok(()); }
        state.counts=counts; state.tick(&self.render,(self.now)())
    }
    pub async fn stop(&mut self)->Result<(),ExtensionFailure> {
        if let Some(timer)=self.timer.take() {
            timer.abort();
            match timer.await { Ok(result)=>result?,Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) }
        }
        let ctx={ let mut state=self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?; let ctx=state.ctx.take(); state.clear(); ctx };
        if let Some(ctx)=ctx && let Err(error)=(self.render)(&ctx,None) && !crate::stale_context::is_stale_extension_context_error(&error) { return Err(error); }
        Ok(())
    }
}
impl Drop for GoalWaitTicker { fn drop(&mut self) { if let Some(timer)=&self.timer { timer.abort(); } } }
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn stale_render_retires_context_and_live_sync_rearms() {
        let mut ticker=GoalWaitTicker::new(Arc::new(|_,_|Err(ExtensionFailure::new(crate::stale_context::STALE_EXTENSION_CONTEXT_ERROR_PREFIX))),Arc::new(||0.0));
        let input=||GoalWaitLabelInput { kind:GoalWaitKind::UserGrace,remaining_ms:10000.0,total_ms:10000.0,channel_counts:Default::default() };
        ticker.sync(crate::test_context::context(),input()).await.unwrap(); assert!(!ticker.running()); assert!(ticker.state.lock().unwrap().ctx.is_none());
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel(); ticker.render=Arc::new(move |_,status| { send.send(status.is_some()).unwrap(); Ok(()) });
        ticker.sync(crate::test_context::context(),input()).await.unwrap(); assert!(receive.recv().await.unwrap()); assert!(ticker.running()); ticker.stop().await.unwrap(); assert!(!receive.recv().await.unwrap());
    }
    #[tokio::test(start_paused=true)] async fn countdown_updates_channels_without_moving_deadline_and_stop_clears() {
        let start=tokio::time::Instant::now(); let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=GoalWaitTicker::new(Arc::new(move |_,status| { send.send(status.map(str::to_owned)).unwrap(); Ok(()) }),Arc::new(move ||(tokio::time::Instant::now()-start).as_secs_f64()*1000.0));
        ticker.sync(crate::test_context::context(),GoalWaitLabelInput { kind:GoalWaitKind::Monitor,remaining_ms:10_000.0,total_ms:10_000.0,channel_counts:ResumptionChannelCounts::new() }).await.unwrap();
        receive.recv().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2),receive.recv()).await.unwrap().unwrap();
        let counts=ResumptionChannelCounts::from([("senpi-task".into(),1.0)]);
        ticker.set_channel_counts(counts.clone()).unwrap();
        assert_eq!(receive.recv().await.unwrap(),Some(format_goal_wait_label(&GoalWaitLabelInput { kind:GoalWaitKind::Monitor,remaining_ms:9000.0,total_ms:10_000.0,channel_counts:counts })));
        ticker.stop().await.unwrap(); assert_eq!(receive.recv().await.unwrap(),None); assert!(!ticker.running());
        drop(ticker); assert!(receive.recv().await.is_none());
    }
    #[tokio::test(start_paused=true)] async fn busy_context_hides_wait_and_idle_signal_reveals_countdown() {
        let idle=Arc::new(std::sync::atomic::AtomicBool::new(false)); let reading=Arc::clone(&idle); let mut ctx=crate::test_context::context(); ctx.is_idle_fn=Arc::new(move ||reading.load(std::sync::atomic::Ordering::SeqCst));
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=GoalWaitTicker::new(Arc::new(move |_,status| { send.send(status.map(str::to_owned)).unwrap(); Ok(()) }),Arc::new(||0.0));
        ticker.sync(ctx,GoalWaitLabelInput { kind:GoalWaitKind::UserGrace,remaining_ms:10_000.0,total_ms:10_000.0,channel_counts:ResumptionChannelCounts::new() }).await.unwrap();
        assert!(receive.try_recv().is_err()); idle.store(true,std::sync::atomic::Ordering::SeqCst);
        assert!(tokio::time::timeout(std::time::Duration::from_secs(2),receive.recv()).await.unwrap().unwrap().is_some());
        ticker.stop().await.unwrap(); assert_eq!(receive.recv().await.unwrap(),None);
    }
    #[tokio::test] async fn stop_before_sync_is_idempotent_and_does_not_render() {
        let mut ticker=GoalWaitTicker::new(Arc::new(|_,_|panic!("no retained context to render")),Arc::new(||0.0));
        ticker.stop().await.unwrap(); ticker.stop().await.unwrap(); assert!(!ticker.running());
    }
}
