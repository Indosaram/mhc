use crate::types::{CronEntry, LoopPhase, LoopState};
pub const LOOP_STATUS_KEY: &str = "loop";
pub const LOOP_STATUS_TICK_INTERVAL_MS: u64 = 1000;
use std::sync::{Arc,Mutex};
use maho_ext_api::ExtensionFailure;
pub type LoopStatusRender=Arc<dyn Fn(&str,Option<&str>)->Result<(),ExtensionFailure>+Send+Sync>;
pub struct LoopStatusTicker {
    state:Arc<Mutex<(Option<LoopState>,Option<String>)>>,
    render:LoopStatusRender,now:Arc<dyn Fn()->f64+Send+Sync>,
    timer:Option<tokio::task::JoinHandle<Result<(),ExtensionFailure>>>,
}
impl LoopStatusTicker {
    pub fn new(render:LoopStatusRender,now:Arc<dyn Fn()->f64+Send+Sync>)->Self { Self { state:Arc::new(Mutex::new((None,None))),render,now,timer:None } }
    pub fn running(&self)->bool { self.timer.as_ref().is_some_and(|timer|!timer.is_finished()) }
    pub async fn sync(&mut self,snapshot:LoopState)->Result<(),ExtensionFailure> {
        self.cancel_timer().await?;
        {
            let mut state=self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
            state.0=Some(snapshot); state.1=None;
            tick_status(&mut state,&self.render,(self.now)())?;
        }
        let state=Arc::clone(&self.state); let render=Arc::clone(&self.render); let now=Arc::clone(&self.now);
        self.timer=Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(LOOP_STATUS_TICK_INTERVAL_MS)).await;
                let mut state=state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                if state.0.is_none() { return Ok(()); }
                tick_status(&mut state,&render,now())?;
            }
        }));
        Ok(())
    }
    async fn cancel_timer(&mut self)->Result<(),ExtensionFailure> {
        if let Some(timer)=self.timer.take() {
            timer.abort();
            match timer.await { Ok(result)=>result?,Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) }
        }
        Ok(())
    }
    pub async fn dispose(&mut self)->Result<(),ExtensionFailure> {
        self.cancel_timer().await?;
        *self.state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?=(None,None);
        (self.render)(LOOP_STATUS_KEY,None)
    }
}
fn tick_status(state:&mut (Option<LoopState>,Option<String>),render:&LoopStatusRender,now:f64)->Result<(),ExtensionFailure> {
    let Some(snapshot)=&state.0 else { return Ok(()); };
    let text=format_loop_status(snapshot,now);
    if text==state.1 { return Ok(()); }
    state.1=text; render(LOOP_STATUS_KEY,state.1.as_deref())
}
impl Drop for LoopStatusTicker { fn drop(&mut self) { if let Some(timer)=&self.timer { timer.abort(); } } }
fn format_duration(ms: f64) -> String {
    let number=maho_ai::utils::js::number_to_string;
    let seconds = if ms.is_nan() { ms } else { (ms / 1000.0).floor().max(0.0) };
    if seconds < 60.0 { return format!("{}s",number(seconds)); }
    let minutes = (seconds / 60.0).floor();
    if minutes < 60.0 { return if seconds % 60.0 > 0.0 { format!("{}m{}s",number(minutes),number(seconds % 60.0)) } else { format!("{}m",number(minutes)) }; }
    let hours = (minutes / 60.0).floor();
    if hours < 24.0 { return if minutes % 60.0 > 0.0 { format!("{}h{}m",number(hours),number(minutes % 60.0)) } else { format!("{}h",number(hours)) }; }
    let days = (hours / 24.0).floor();
    if hours % 24.0 > 0.0 { format!("{}d{}h",number(days),number(hours % 24.0)) } else { format!("{}d",number(days)) }
}
pub fn format_loop_status(state: &LoopState, now_ms: f64) -> Option<String> {
    let armed: Vec<_> = state.entries.values().filter(|entry| match entry { CronEntry::Fixed { lifecycle, .. } | CronEntry::Dynamic { lifecycle, .. } => lifecycle.phase != LoopPhase::Ended }).collect();
    if armed.is_empty() { return None; }
    if armed.iter().any(|entry| match entry { CronEntry::Fixed { lifecycle, .. } | CronEntry::Dynamic { lifecycle, .. } => lifecycle.phase == LoopPhase::Suspended }) { return Some("Loop paused - /loop resume or /loop stop".into()); }
    let mut nearest = None;
    for entry in armed {
        let (mode, due) = match entry { CronEntry::Fixed { next_fire_at, .. } => ("fixed",Some(*next_fire_at)), CronEntry::Dynamic { pending_wakeup, .. } => ("dynamic",pending_wakeup.as_ref().map(|w| w.due_at)) };
        if let Some(due) = due && nearest.is_none_or(|(_, previous)| due < previous) { nearest = Some((mode,due)); }
    }
    Some(nearest.map_or_else(|| "Loop active - /loop stop".into(), |(mode,due)| format!("Loop ({mode}): next in {} - /loop stop",format_duration((due-now_ms).max(0.0)))))
}
pub fn format_noop_fold(noop_streak: f64) -> String { if noop_streak < 2.0 { String::new() } else { format!("\u{21bb} {} loop ticks with no actionable change",maho_ai::utils::js::number_to_string(noop_streak)) } }
#[cfg(test)] mod tests {
    use super::*;
    fn state(phase: &str, due: f64) -> LoopState { serde_json::from_value(serde_json::json!({"version":1,"sessionId":"s","updatedAt":0,"activeDynamicId":null,"entries":{"a":{"id":"a","kind":"fixed","phase":phase,"originalArgs":"5m check","reentryPrompt":"/loop 5m check","payload":{"type":"prompt","prompt":"check"},"createdAt":0,"lastFiredAt":null,"expiresAt":1000000000000.0,"lastScheduledForAt":null,"coalescedFirePending":false,"queuedForAt":null,"noopStreak":0,"tickCount":0,"sentinelDelivery":{"autonomousPreambleDelivered":false,"lastLoopFileDelivered":null,"forceFullDelivery":false},"wakeSources":[],"requestedInterval":{"value":5,"unit":"m","raw":"5m"},"effectiveInterval":{"value":5,"unit":"m","human":"5 minutes","rounded":false},"cronExpression":"*/5 * * * *","nextFireAt":due,"intervalMs":300000}}})).unwrap() }
    #[test] fn nothing_armed_has_no_status() { let state=state("ended",60000.0); let result=format_loop_status(&state,0.0); assert!(result.is_none()); }
    #[test] fn fixed_countdown_is_selected() { let state=state("waiting",60000.0); let result=format_loop_status(&state,0.0).unwrap(); assert!(result.contains("fixed")); assert!(result.contains("1m")); }
    #[test] fn countdown_uses_supplied_clock() { let state=state("waiting",300000.0); let result=format_loop_status(&state,150000.0).unwrap(); assert!(result.contains("2m30s")); }
    #[test] fn dynamic_wakeup_countdown_is_selected() { let mut value=serde_json::to_value(state("waiting",0.0)).unwrap(); value["entries"]["a"]["kind"]=serde_json::json!("dynamic"); value["entries"]["a"]["keepaliveCredit"]=serde_json::json!(1); value["entries"]["a"]["pendingWakeup"]=serde_json::json!({"id":"w","loopId":"a","kind":"dynamic","source":"model","requestedDelaySeconds":120,"delaySeconds":120,"dueAt":120000,"reason":"continue","prompt":"/loop check","noop":false,"createdAt":0}); let state=serde_json::from_value(value).unwrap(); let result=format_loop_status(&state,0.0).unwrap(); assert!(result.contains("dynamic")); assert!(result.contains("2m")); }
    #[test] fn suspended_loop_has_pause_status() { let state=state("suspended",60000.0); let result=format_loop_status(&state,0.0).unwrap(); assert!(result.contains("paused")); }
    #[test] fn noops_below_two_are_not_folded() { let result=format_noop_fold(0.0); assert!(result.is_empty()); }
    #[test] fn noop_streak_is_exposed() { let result=format_noop_fold(3.0); assert!(result.contains('3')); }
    #[test] fn noop_streak_preserves_javascript_exponent_number() { assert!(format_noop_fold(1e21).contains("1e+21")); assert!(format_noop_fold(f64::NAN).contains("NaN")); }
    #[test] fn duration_retains_day_and_hour_parts() { let result=format_duration(90000000.0); assert_eq!(result,"1d1h"); }
    #[test] fn duration_preserves_javascript_nonfinite_and_exponent_spelling() { assert_eq!(format_duration(f64::NAN),"NaNd"); assert_eq!(format_duration(f64::INFINITY),"Infinityd"); assert_eq!(format_duration(f64::NEG_INFINITY),"0s"); assert!(format_duration(1e30).contains("e+")); }
    #[tokio::test(start_paused=true)] async fn ticker_updates_countdown_and_dispose_clears_status() {
        let start=tokio::time::Instant::now();
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=LoopStatusTicker::new(Arc::new(move |key,text| { assert_eq!(key,LOOP_STATUS_KEY); send.send(text.map(str::to_owned)).unwrap(); Ok(()) }),Arc::new(move ||(tokio::time::Instant::now()-start).as_secs_f64()*1000.0));
        let snapshot=state("waiting",60_000.0);
        ticker.sync(snapshot.clone()).await.unwrap();
        assert_eq!(receive.recv().await.unwrap(),format_loop_status(&snapshot,0.0));
        let next=receive.recv();
        let rendered=tokio::time::timeout(std::time::Duration::from_secs(2),next).await.unwrap().unwrap();
        assert_eq!(rendered,format_loop_status(&snapshot,1000.0));
        ticker.dispose().await.unwrap();
        assert_eq!(receive.recv().await.unwrap(),None); assert!(!ticker.running());
        drop(ticker); assert!(receive.recv().await.is_none());
    }
    #[tokio::test] async fn dispose_before_sync_clears_status_without_timer() {
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel();
        let mut ticker=LoopStatusTicker::new(Arc::new(move |_,text| { send.send(text.map(str::to_owned)).unwrap(); Ok(()) }),Arc::new(||0.0));
        ticker.dispose().await.unwrap(); assert_eq!(receive.recv().await.unwrap(),None); assert!(!ticker.running());
    }
}
