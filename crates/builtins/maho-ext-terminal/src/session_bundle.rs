use std::sync::{Arc,Mutex};
use crate::{manager::TerminalManager,monitor_registry::{MonitorRegistry,MonitorEvent}};
type MonitorSink=Arc<dyn Fn(MonitorEvent)+Send+Sync>;
#[derive(Default)]
struct Routing {sink:Option<MonitorSink>,parked:std::collections::VecDeque<MonitorEvent>,torndown:bool}
pub struct TerminalSessionBundle {pub manager:TerminalManager,pub monitors:MonitorRegistry,routing:Arc<Mutex<Routing>>}
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
