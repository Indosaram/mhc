use std::sync::{Arc,Mutex};
use crate::{manager::TerminalManager,monitor_registry::{MonitorRegistry,MonitorEvent}};
type MonitorSink=Arc<dyn Fn(MonitorEvent)+Send+Sync>;
pub type BackgroundStateSink=Arc<dyn Fn(&[BackgroundSession])+Send+Sync>;
pub type BackgroundExitSink=Arc<dyn Fn(&str,&crate::runtime_session::TerminalRuntimeSession)+Send+Sync>;
#[derive(Clone,Debug,PartialEq)]
pub struct BackgroundSession {pub id:String,pub description:String,pub started_at_ms:f64}
#[derive(Default)]
struct Routing {sink:Option<MonitorSink>,parked:std::collections::VecDeque<MonitorEvent>,torndown:bool}
pub struct TerminalSessionBundle {pub manager:TerminalManager,pub monitors:MonitorRegistry,routing:Arc<Mutex<Routing>>,backgrounds:indexmap::IndexMap<String,BackgroundSession>,parked_exits:indexmap::IndexSet<String>,background_state:Option<BackgroundStateSink>,background_exit:Option<BackgroundExitSink>}
fn parked_bundles()->&'static Mutex<std::collections::BTreeMap<String,Arc<Mutex<TerminalSessionBundle>>>> {
    static BUNDLES:std::sync::OnceLock<Mutex<std::collections::BTreeMap<String,Arc<Mutex<TerminalSessionBundle>>>>>=std::sync::OnceLock::new();
    BUNDLES.get_or_init(Mutex::default)
}
pub fn park_bundle(session_key:&str,bundle:Arc<Mutex<TerminalSessionBundle>>)->Result<(),crate::runtime_session::RuntimeError> {
    bundle.lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.park();
    let previous=parked_bundles().lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.insert(session_key.to_owned(),bundle.clone());
    if let Some(previous)=previous && !Arc::ptr_eq(&previous,&bundle) {previous.lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.teardown()?;}
    Ok(())
}
pub fn claim_parked_bundle(session_key:&str)->Option<Arc<Mutex<TerminalSessionBundle>>> {parked_bundles().lock().expect("parked bundles").remove(session_key)}
pub fn teardown_parked_bundle(session_key:&str)->Result<(),crate::runtime_session::RuntimeError> {if let Some(bundle)=claim_parked_bundle(session_key) {bundle.lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.teardown()?;}Ok(())}
impl TerminalSessionBundle {
    pub fn new(max_sessions:usize)->Self {
        let routing=Arc::new(Mutex::new(Routing::default()));let dispatch=routing.clone();
        let monitors=MonitorRegistry::new(move |event| {
            let sink={
                let mut routing=dispatch.lock().expect("terminal routing");
                if routing.torndown {return;}
                if let Some(sink)=&routing.sink {Some(sink.clone())} else {routing.parked.push_back(event.clone());if routing.parked.len()>100 {routing.parked.pop_front();}None}
            };
            if let Some(sink)=sink {sink(event);}
        });
        Self {manager:TerminalManager::new(max_sessions),monitors,routing,backgrounds:Default::default(),parked_exits:Default::default(),background_state:None,background_exit:None}
    }
    pub fn bind(&mut self,sink:MonitorSink) {
        let events={let mut routing=self.routing.lock().expect("terminal routing");if routing.torndown {return;}routing.sink=Some(sink.clone());std::mem::take(&mut routing.parked)};
        for event in events {sink(event);}
    }
    pub fn bind_backgrounds(&mut self,state:BackgroundStateSink,exit:BackgroundExitSink) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        self.background_state=Some(state.clone());self.background_exit=Some(exit.clone());state(&self.background_snapshot());
        for id in std::mem::take(&mut self.parked_exits) {if let Some(runtime)=self.manager.get(&id) {exit(&id,runtime);}}
    }
    pub fn background_snapshot(&self)->Vec<BackgroundSession> {self.backgrounds.values().cloned().collect()}
    pub fn notify_background_start(&mut self,id:&str,description:&str,started_at_ms:f64) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        self.backgrounds.insert(id.to_owned(),BackgroundSession {id:id.to_owned(),description:description.to_owned(),started_at_ms});if let Some(state)=&self.background_state {state(&self.background_snapshot());}
    }
    pub fn notify_background_exit(&mut self,id:&str) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        if self.backgrounds.shift_remove(id).is_some()&&let Some(state)=&self.background_state {state(&self.background_snapshot());}
        if let Some(exit)=&self.background_exit {if let Some(runtime)=self.manager.get(id) {exit(id,runtime);}return;}
        if self.parked_exits.len()<32 {self.parked_exits.insert(id.to_owned());}
    }
    pub fn park(&mut self) {self.routing.lock().expect("terminal routing").sink=None;self.background_state=None;self.background_exit=None;}
    pub fn teardown(&mut self)->Result<(),crate::runtime_session::RuntimeError> {
        {let mut routing=self.routing.lock().expect("terminal routing");if routing.torndown {return Ok(());}routing.torndown=true;routing.sink=None;routing.parked.clear();}
        self.parked_exits.clear();if !self.backgrounds.is_empty() {self.backgrounds.clear();if let Some(state)=&self.background_state {state(&[]);}}self.background_state=None;self.background_exit=None;
        self.monitors.dispose();self.manager.teardown()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parked_background_exit_republishes_state_and_flushes_to_new_owner() {
        let mut bundle=TerminalSessionBundle::new(2);let id=bundle.manager.create("ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();bundle.manager.get(&id).unwrap().wait(std::time::Duration::from_secs(5)).unwrap();
        bundle.notify_background_start(&id,"command",1.0);assert_eq!(bundle.background_snapshot()[0].id,id);bundle.park();bundle.notify_background_exit(&id);assert!(bundle.background_snapshot().is_empty());
        let (sender,receiver)=std::sync::mpsc::channel();let state_sender=sender.clone();bundle.bind_backgrounds(Arc::new(move |state| {state_sender.send(format!("state {}",state.len())).unwrap();}),Arc::new(move |id,runtime| {sender.send(format!("exit {id} {}",runtime.full_output().unwrap().trim())).unwrap();}));assert_eq!(receiver.try_recv().unwrap(),"state 0");assert_eq!(receiver.try_recv().unwrap(),format!("exit {id} ready"));assert!(receiver.try_recv().is_err());assert!(bundle.parked_exits.is_empty());bundle.teardown().unwrap();
    }
    #[test]
    fn claim_transfers_same_bundle_only_once() {
        let bundle=Arc::new(Mutex::new(TerminalSessionBundle::new(1)));park_bundle("bundle-claim-test",bundle.clone()).unwrap();
        assert!(Arc::ptr_eq(&claim_parked_bundle("bundle-claim-test").unwrap(),&bundle));assert!(claim_parked_bundle("bundle-claim-test").is_none());bundle.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn parked_monitor_completion_flushes_into_new_owner() {
        let mut bundle=TerminalSessionBundle::new(1);
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        bundle.monitors.register(&runtime,crate::monitor_registry::CommandMonitor::new(crate::monitor_registry::MonitorSnapshotEntry {id:"bash_1".to_owned(),..Default::default()},None)).unwrap();
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();bundle.bind(Arc::new(move |event| {sender.send(event).unwrap();}));
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(MonitorEvent::Line {..})));assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {..})));}).await.unwrap();bundle.teardown().unwrap();runtime.dispose().unwrap();
    }
}
