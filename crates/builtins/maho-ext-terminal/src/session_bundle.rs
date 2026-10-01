use std::sync::{Arc,Mutex};
use crate::{manager::TerminalManager,monitor_registry::{MonitorRegistry,MonitorEvent}};
type MonitorSink=Arc<dyn Fn(MonitorEvent)+Send+Sync>;
#[derive(Default)]
struct Routing {sink:Option<MonitorSink>,parked:std::collections::VecDeque<MonitorEvent>,torndown:bool}
pub struct TerminalSessionBundle {pub manager:TerminalManager,pub monitors:MonitorRegistry,routing:Arc<Mutex<Routing>>}
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
        Self {manager:TerminalManager::new(max_sessions),monitors,routing}
    }
    pub fn bind(&mut self,sink:MonitorSink) {
        let events={let mut routing=self.routing.lock().expect("terminal routing");if routing.torndown {return;}routing.sink=Some(sink.clone());std::mem::take(&mut routing.parked)};
        for event in events {sink(event);}
    }
    pub fn park(&mut self) {self.routing.lock().expect("terminal routing").sink=None;}
    pub fn teardown(&mut self)->Result<(),crate::runtime_session::RuntimeError> {
        {let mut routing=self.routing.lock().expect("terminal routing");if routing.torndown {return Ok(());}routing.torndown=true;routing.sink=None;routing.parked.clear();}
        self.monitors.dispose();self.manager.teardown()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
