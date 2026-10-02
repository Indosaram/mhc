use crate::monitor_registry::MonitorSnapshotEntry;
use crate::monitor_status::format_monitor_status;
pub const MONITOR_STATUS_TICK_INTERVAL_MS:u64=1000;
#[derive(Default)]
pub struct MonitorStatusTicker {snapshot:Vec<MonitorSnapshotEntry>,last_rendered_status:Option<String>,has_rendered:bool,running:bool}
impl MonitorStatusTicker {
    pub fn running(&self)->bool {self.running}
    pub fn sync(&mut self,snapshot:Vec<MonitorSnapshotEntry>,now:f64,render:impl FnMut(Option<String>)) {self.snapshot=snapshot;self.has_rendered=false;self.tick(now,render);self.running = !self.snapshot.is_empty();}
    pub fn stop(&mut self) {self.running=false;self.snapshot.clear();self.last_rendered_status=None;self.has_rendered=false;}
    pub fn tick(&mut self,now:f64,mut render:impl FnMut(Option<String>)) {let status=format_monitor_status(&self.snapshot,now);if self.has_rendered&&status==self.last_rendered_status {return;}self.has_rendered=true;self.last_rendered_status=status.clone();render(status);}
}
pub struct ScheduledMonitorStatusTicker {
    state:std::sync::Arc<std::sync::Mutex<MonitorStatusTicker>>,
    task:Option<tokio::task::JoinHandle<()>>,
    render:std::sync::Arc<dyn Fn(Option<String>)+Send+Sync>,
    now:std::sync::Arc<dyn Fn()->f64+Send+Sync>,
}
impl ScheduledMonitorStatusTicker {
    pub fn new(render:impl Fn(Option<String>)+Send+Sync+'static,now:impl Fn()->f64+Send+Sync+'static)->Self {Self {state:Default::default(),task:None,render:std::sync::Arc::new(render),now:std::sync::Arc::new(now)}}
    pub fn sync(&mut self,snapshot:Vec<MonitorSnapshotEntry>) {
        self.state.lock().expect("monitor ticker").sync(snapshot,(self.now)(),|status|(self.render)(status));
        if !self.state.lock().expect("monitor ticker").running() {if let Some(task)=self.task.take() {task.abort();}return;}
        if self.task.is_some() {return;}
        let state=self.state.clone();let now=self.now.clone();let render=self.render.clone();
        self.task=Some(tokio::spawn(async move {
            let period=std::time::Duration::from_millis(MONITOR_STATUS_TICK_INTERVAL_MS);let mut interval=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {interval.tick().await;state.lock().expect("monitor ticker").tick(now(),|status|render(status));}
        }));
    }
    pub fn stop(&mut self) {if let Some(task)=self.task.take() {task.abort();}self.state.lock().expect("monitor ticker").stop();}
}
impl Drop for ScheduledMonitorStatusTicker {fn drop(&mut self) {self.stop();}}
pub fn bind_monitor_status(mut state:tokio::sync::watch::Receiver<Vec<MonitorSnapshotEntry>>,render:impl Fn(Option<String>)+Send+Sync+'static)->tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker=ScheduledMonitorStatusTicker::new(render,||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0);
        ticker.sync(state.borrow_and_update().clone());
        while state.changed().await.is_ok() {ticker.sync(state.borrow_and_update().clone());}
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn registry_transitions_update_and_clear_bound_status() {
        let dir=tempfile::tempdir().unwrap();let mut registry=crate::monitor_registry::MonitorRegistry::new(|_|{});let (sender,mut rendered)=tokio::sync::mpsc::unbounded_channel();
        let task=bind_monitor_status(registry.subscribe_state(),move |status| {sender.send(status).unwrap();});
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            assert_eq!(rendered.recv().await.unwrap(),None);
            let (id,_)=registry.register_persistent_file("watch",&dir.path().join("file"),crate::terminal_manifest_model::FileEvent::Create).unwrap();assert!(rendered.recv().await.unwrap().is_some());
            registry.stop_file(&id);assert_eq!(rendered.recv().await.unwrap(),None);
        }).await.unwrap();task.abort();assert!(task.await.unwrap_err().is_cancelled());
    }
    #[tokio::test(start_paused=true)]
    async fn scheduled_elapsed_refresh_has_exact_timer_cadence() {
        let now=std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));let clock=now.clone();
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut ticker=ScheduledMonitorStatusTicker::new(move |status| {sender.send(status).unwrap();},move ||clock.load(std::sync::atomic::Ordering::SeqCst) as f64);
        ticker.sync(vec![MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"watch".to_owned(),..Default::default()}]);
        let initial=events.recv().await.unwrap();tokio::task::yield_now().await;now.store(1000,std::sync::atomic::Ordering::SeqCst);tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert_ne!(events.recv().await.unwrap(),initial);ticker.sync(vec![]);assert_eq!(events.recv().await.unwrap(),None);assert!(ticker.task.is_none());ticker.stop();
    }
    #[test] fn live_tick_deduplicates_and_stop_drops_snapshot() {let mut ticker=MonitorStatusTicker::default();let mut rendered=Vec::new();ticker.sync(vec![MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"build".to_owned(),started_at_ms:0.0,..Default::default()}],0.0,|status|rendered.push(status));assert!(ticker.running());ticker.tick(0.0,|status|rendered.push(status));assert_eq!(rendered.len(),1);ticker.tick(1000.0,|status|rendered.push(status));assert_eq!(rendered.len(),2);ticker.stop();assert!(!ticker.running());ticker.sync(Vec::new(),2000.0,|status|rendered.push(status));assert_eq!(rendered.last(),Some(&None));}
}
