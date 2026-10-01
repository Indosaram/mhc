use crate::types::Goal;
use maho_ext_api::{ExtensionContext,ExtensionFailure};
use std::sync::{Arc,Mutex};
pub type GoalElapsedRender=Arc<dyn Fn(&ExtensionContext,&Goal,f64)->Result<(),ExtensionFailure>+Send+Sync>;
pub type GoalElapsedClock=Arc<dyn Fn()->f64+Send+Sync>;
struct ElapsedState { ctx:Option<ExtensionContext>,goal:Option<Goal>,measured_from:f64,last_label:Option<String> }
impl ElapsedState {
    fn tick(&mut self,render:&GoalElapsedRender,now:f64)->Result<(),ExtensionFailure> {
        let (Some(ctx),Some(goal))=(&self.ctx,&self.goal) else { return Ok(()); };
        let live=goal_live_elapsed_seconds(goal,self.measured_from,now);
        let label=crate::format::format_goal_elapsed_seconds(live);
        if self.last_label.as_ref()==Some(&label) { return Ok(()); }
        self.last_label=Some(label);
        if let Err(error)=render(ctx,goal,live) {
            if crate::stale_context::is_stale_extension_context_error(&error) { self.clear(); } else { return Err(error); }
        }
        Ok(())
    }
    fn clear(&mut self) { self.ctx=None; self.goal=None; self.last_label=None; }
}
pub struct GoalElapsedTicker {
    state:Arc<Mutex<ElapsedState>>,render:GoalElapsedRender,now:GoalElapsedClock,
    timer:Option<tokio::task::JoinHandle<Result<(),ExtensionFailure>>>,
}
impl GoalElapsedTicker {
    pub fn new(render:GoalElapsedRender,now:GoalElapsedClock)->Self {
        Self { state:Arc::new(Mutex::new(ElapsedState { ctx:None,goal:None,measured_from:0.0,last_label:None })),render,now,timer:None }
    }
    pub fn running(&self)->bool { self.timer.as_ref().is_some_and(|timer|!timer.is_finished()) }
    pub async fn sync(&mut self,ctx:ExtensionContext,goal:Goal,measured_from:f64)->Result<(),ExtensionFailure> {
        if self.timer.as_ref().is_some_and(tokio::task::JoinHandle::is_finished) && let Some(timer)=self.timer.take() {
            timer.await.map_err(|error|ExtensionFailure::new(error.to_string()))??;
        }
        {
            let mut state=self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
            state.ctx=Some(ctx); state.goal=Some(goal); state.measured_from=measured_from; state.last_label=None;
            state.tick(&self.render,(self.now)())?;
            if state.ctx.is_none() { return Ok(()); }
        }
        if self.timer.is_some() { return Ok(()); }
        let state=Arc::clone(&self.state); let render=Arc::clone(&self.render); let now=Arc::clone(&self.now);
        self.timer=Some(tokio::spawn(async move {
            let mut interval=tokio::time::interval_at(tokio::time::Instant::now()+std::time::Duration::from_millis(GOAL_ELAPSED_TICK_INTERVAL_MS),std::time::Duration::from_millis(GOAL_ELAPSED_TICK_INTERVAL_MS));
            loop {
                interval.tick().await;
                let mut state=state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                state.tick(&render,now())?;
                if state.ctx.is_none() { return Ok(()); }
            }
        }));
        Ok(())
    }
    pub fn stop(&mut self)->Result<Option<tokio::task::JoinHandle<Result<(),ExtensionFailure>>>,ExtensionFailure> {
        let timer=self.timer.take(); if let Some(timer)=&timer { timer.abort(); }
        self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.clear();
        Ok(timer)
    }
}
impl Drop for GoalElapsedTicker { fn drop(&mut self) { if let Some(timer)=&self.timer { timer.abort(); } } }
pub const GOAL_ELAPSED_TICK_INTERVAL_MS:u64=1000;
pub fn goal_live_elapsed_seconds(goal:&Goal,measured_from_milliseconds:f64,now_milliseconds:f64)->f64 {
    goal.time_used_seconds+((now_milliseconds-measured_from_milliseconds)/1000.0+0.5).floor().max(0.0)
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn stale_context_retires_and_live_resync_rearms() {
        let stale=Arc::new(std::sync::atomic::AtomicBool::new(true)); let rendering=Arc::clone(&stale);
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=GoalElapsedTicker::new(Arc::new(move |_,_,live| {
            if rendering.load(std::sync::atomic::Ordering::SeqCst) { return Err(ExtensionFailure::new(crate::stale_context::STALE_EXTENSION_CONTEXT_ERROR_PREFIX)); }
            send.send(live).unwrap(); Ok(())
        }),Arc::new(||0.0));
        ticker.sync(crate::test_context::context(),goal(),0.0).await.unwrap(); assert!(!ticker.running());
        stale.store(false,std::sync::atomic::Ordering::SeqCst);
        ticker.sync(crate::test_context::context(),goal(),0.0).await.unwrap(); assert_eq!(receive.recv().await.unwrap(),10.0); assert!(ticker.running());
        ticker.stop().unwrap().unwrap().await.unwrap_err();
    }
    #[tokio::test(start_paused=true)] async fn live_elapsed_advances_and_stop_observes_worker_cleanup() {
        let start=tokio::time::Instant::now(); let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=GoalElapsedTicker::new(Arc::new(move |_,_,live| { send.send(live).unwrap(); Ok(()) }),Arc::new(move ||(tokio::time::Instant::now()-start).as_secs_f64()*1000.0));
        ticker.sync(crate::test_context::context(),goal(),0.0).await.unwrap(); assert_eq!(receive.recv().await.unwrap(),10.0);
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2),receive.recv()).await.unwrap().unwrap(),11.0);
        let worker=ticker.stop().unwrap().unwrap(); assert!(worker.await.unwrap_err().is_cancelled()); assert!(!ticker.running());
        drop(ticker); assert!(receive.recv().await.is_none());
    }
    #[tokio::test] async fn resync_replaces_goal_snapshot_and_measurement_window() {
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=GoalElapsedTicker::new(Arc::new(move |_,goal,live| { send.send((goal.id.clone(),live)).unwrap(); Ok(()) }),Arc::new(||1000.0));
        ticker.sync(crate::test_context::context(),goal(),0.0).await.unwrap(); assert_eq!(receive.recv().await.unwrap(),("g".into(),11.0));
        let mut replacement=goal(); replacement.id="next".into(); replacement.time_used_seconds=20.0;
        ticker.sync(crate::test_context::context(),replacement,1000.0).await.unwrap(); assert_eq!(receive.recv().await.unwrap(),("next".into(),20.0));
        ticker.stop().unwrap().unwrap().await.unwrap_err();
    }
    fn goal()->Goal { serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":"active","tokensUsed":0,"timeUsedSeconds":10.0,"createdAt":0,"updatedAt":0})).unwrap() }
    #[test] fn live_elapsed_rounds_positive_half_seconds() { let goal=goal(); let result=[goal_live_elapsed_seconds(&goal,1000.0,1499.0),goal_live_elapsed_seconds(&goal,1000.0,1500.0)]; assert_eq!(result,[10.0,11.0]); }
    #[test] fn backward_clock_never_subtracts_committed_elapsed() { let result=goal_live_elapsed_seconds(&goal(),1000.0,0.0); assert_eq!(result,10.0); }
    #[test] fn stopping_before_sync_is_idempotent() {
        let mut ticker=GoalElapsedTicker::new(Arc::new(|_,_,_|Ok(())),Arc::new(||0.0));
        assert!(ticker.stop().unwrap().is_none()); assert!(ticker.stop().unwrap().is_none()); assert!(!ticker.running());
    }
}
