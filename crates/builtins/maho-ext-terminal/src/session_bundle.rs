use std::sync::{Arc,Mutex};
use crate::{manager::TerminalManager,monitor_registry::{MonitorRegistry,MonitorEvent,MonitorEndedEvent,MonitorSnapshotEntry}};
type MonitorSink=Arc<dyn Fn(MonitorEvent)+Send+Sync>;
type EndedSink=Arc<dyn Fn(MonitorEndedEvent)+Send+Sync>;
type StateSink=Arc<dyn Fn(&[MonitorSnapshotEntry],bool)+Send+Sync>;
pub type BackgroundStateSink=Arc<dyn Fn(&[BackgroundSession])+Send+Sync>;
pub type BackgroundExitSink=Arc<dyn Fn(&str,&crate::runtime_session::TerminalRuntimeSession)+Send+Sync>;
#[derive(Clone,Debug,PartialEq)]
pub struct BackgroundSession {pub id:String,pub description:String,pub started_at_ms:f64}
#[derive(Clone)]
pub struct TerminalEventSinks {pub on_monitor_event:MonitorSink,pub on_monitor_state:StateSink,pub on_monitor_ended:EndedSink,pub on_background_state:BackgroundStateSink,pub on_background_exit:BackgroundExitSink}
#[derive(Default)]
struct Routing {sink:Option<MonitorSink>,parked:std::collections::VecDeque<MonitorEvent>,torndown:bool}
#[derive(Default)]
struct EndedRouting {sink:Option<EndedSink>,parked:std::collections::VecDeque<MonitorEndedEvent>}
#[derive(Clone)]
pub struct TerminalSessionBundle {pub manager:Arc<Mutex<TerminalManager>>,pub monitors:Arc<Mutex<MonitorRegistry>>,pub backgrounds:Arc<Mutex<indexmap::IndexMap<String,BackgroundSession>>>,routing:Arc<Mutex<Routing>>,ended_routing:Arc<Mutex<EndedRouting>>,parked_exits:Arc<Mutex<indexmap::IndexSet<String>>>,state_sink:Option<StateSink>,background_state:Option<BackgroundStateSink>,background_exit:Option<BackgroundExitSink>}
fn parked_bundles()->&'static Mutex<std::collections::BTreeMap<String,TerminalSessionBundle>> {
    static BUNDLES:std::sync::OnceLock<Mutex<std::collections::BTreeMap<String,TerminalSessionBundle>>>=std::sync::OnceLock::new();
    BUNDLES.get_or_init(Mutex::default)
}
pub fn park_bundle(session_key:&str,mut bundle:TerminalSessionBundle)->Result<(),crate::runtime_session::RuntimeError> {
    bundle.park();
    let previous=parked_bundles().lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.insert(session_key.to_owned(),bundle);
    if let Some(mut previous)=previous {previous.teardown()?;}
    Ok(())
}
pub fn claim_parked_bundle(session_key:&str)->Option<TerminalSessionBundle> {parked_bundles().lock().expect("parked bundles").remove(session_key)}
pub fn teardown_parked_bundle(session_key:&str)->Result<(),crate::runtime_session::RuntimeError> {if let Some(mut bundle)=claim_parked_bundle(session_key) {bundle.teardown()?;}Ok(())}
impl TerminalSessionBundle {
    pub fn new(max_sessions:usize)->Self {
        let manager=Arc::new(Mutex::new(TerminalManager::new(max_sessions)));
        let backgrounds=Arc::new(Mutex::new(indexmap::IndexMap::new()));
        let routing=Arc::new(Mutex::new(Routing::default()));let dispatch=routing.clone();
        let ended_routing=Arc::new(Mutex::new(EndedRouting::default()));let ended_dispatch=ended_routing.clone();
        let monitors=Arc::new(Mutex::new(MonitorRegistry::new_with_ended(move |event| {
            let sink={
                let mut routing=dispatch.lock().expect("terminal routing");
                if routing.torndown {return;}
                if let Some(sink)=&routing.sink {Some(sink.clone())} else {routing.parked.push_back(event.clone());if routing.parked.len()>100 {routing.parked.pop_front();}None}
            };
            if let Some(sink)=sink {sink(event);}
        },move |event| {
            let sink={let mut routing=ended_dispatch.lock().expect("terminal ended routing");if let Some(sink)=&routing.sink {Some(sink.clone())} else {routing.parked.push_back(event.clone());if routing.parked.len()>32 {routing.parked.pop_front();}None}};
            if let Some(sink)=sink {sink(event);}
        })));
        Self {manager,monitors,backgrounds,routing,ended_routing,parked_exits:Arc::new(Mutex::new(indexmap::IndexSet::new())),state_sink:None,background_state:None,background_exit:None}
    }
    pub fn from_parts(manager:Arc<Mutex<TerminalManager>>,monitors:Arc<Mutex<MonitorRegistry>>,backgrounds:Arc<Mutex<indexmap::IndexMap<String,BackgroundSession>>>)->Self {
        Self {manager,monitors,backgrounds,routing:Arc::new(Mutex::new(Routing::default())),ended_routing:Arc::new(Mutex::new(EndedRouting::default())),parked_exits:Arc::new(Mutex::new(indexmap::IndexSet::new())),state_sink:None,background_state:None,background_exit:None}
    }
    pub fn adopt(&self,source:&TerminalSessionBundle) {
        *self.manager.lock().expect("terminal manager")=std::mem::take(&mut *source.manager.lock().expect("terminal manager"));
        *self.monitors.lock().expect("monitor registry")=std::mem::replace(&mut *source.monitors.lock().expect("monitor registry"),MonitorRegistry::new(|_|{}));
        *self.backgrounds.lock().expect("terminal backgrounds")=std::mem::take(&mut *source.backgrounds.lock().expect("terminal backgrounds"));
    }
    pub fn bind(&mut self,sink:MonitorSink) {
        let events={let mut routing=self.routing.lock().expect("terminal routing");if routing.torndown {return;}routing.sink=Some(sink.clone());std::mem::take(&mut routing.parked)};
        for event in events {sink(event);}
    }
    pub fn bind_sinks(&mut self,sinks:TerminalEventSinks) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        self.bind(sinks.on_monitor_event.clone());
        {let mut routing=self.ended_routing.lock().expect("terminal ended routing");routing.sink=Some(sinks.on_monitor_ended.clone());}
        self.state_sink=Some(sinks.on_monitor_state.clone());
        self.bind_backgrounds(sinks.on_background_state,sinks.on_background_exit);
        let events=std::mem::take(&mut self.ended_routing.lock().expect("terminal ended routing").parked);
        for event in events {(sinks.on_monitor_ended)(event);}
    }
    pub fn bind_backgrounds(&mut self,state:BackgroundStateSink,exit:BackgroundExitSink) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        self.background_state=Some(state.clone());self.background_exit=Some(exit.clone());state(&self.background_snapshot());
        let parked=std::mem::take(&mut *self.parked_exits.lock().expect("parked exits"));
        for id in parked {if let Some(runtime)=self.manager.lock().expect("terminal manager").get(&id) {exit(&id,runtime);}}
    }
    pub fn publish_state(&self,snapshot:&[MonitorSnapshotEntry],transition:bool) {if let Some(sink)=&self.state_sink {sink(snapshot,transition);}}
    pub fn background_snapshot(&self)->Vec<BackgroundSession> {self.backgrounds.lock().expect("terminal backgrounds").values().cloned().collect()}
    pub fn notify_background_start(&mut self,id:&str,description:&str,started_at_ms:f64) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        self.backgrounds.lock().expect("terminal backgrounds").insert(id.to_owned(),BackgroundSession {id:id.to_owned(),description:description.to_owned(),started_at_ms});if let Some(state)=&self.background_state {state(&self.background_snapshot());}
    }
    pub fn notify_background_exit(&mut self,id:&str) {
        if self.routing.lock().expect("terminal routing").torndown {return;}
        let removed=self.backgrounds.lock().expect("terminal backgrounds").shift_remove(id).is_some();
        if removed&&let Some(state)=&self.background_state {state(&self.background_snapshot());}
        if let Some(exit)=&self.background_exit {if let Some(runtime)=self.manager.lock().expect("terminal manager").get(id) {exit(id,runtime);}return;}
        let mut parked=self.parked_exits.lock().expect("parked exits");if parked.len()<32 {parked.insert(id.to_owned());}
    }
    pub fn park(&mut self) {self.routing.lock().expect("terminal routing").sink=None;self.ended_routing.lock().expect("terminal ended routing").sink=None;self.state_sink=None;self.background_state=None;self.background_exit=None;}
    pub fn teardown(&mut self)->Result<(),crate::runtime_session::RuntimeError> {
        {let mut routing=self.routing.lock().expect("terminal routing");if routing.torndown {return Ok(());}routing.torndown=true;routing.sink=None;routing.parked.clear();}
        self.ended_routing.lock().expect("terminal ended routing").parked.clear();
        self.parked_exits.lock().expect("parked exits").clear();
        let had_backgrounds=!self.backgrounds.lock().expect("terminal backgrounds").is_empty();
        if had_backgrounds {self.backgrounds.lock().expect("terminal backgrounds").clear();if let Some(state)=&self.background_state {state(&[]);}}self.state_sink=None;self.background_state=None;self.background_exit=None;
        self.monitors.lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.dispose();self.manager.lock().map_err(|_|crate::runtime_session::RuntimeError::Poisoned)?.teardown()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parked_background_exit_republishes_state_and_flushes_to_new_owner() {
        let mut bundle=TerminalSessionBundle::new(2);let id=bundle.manager.lock().unwrap().create("ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();bundle.manager.lock().unwrap().get(&id).unwrap().wait(std::time::Duration::from_secs(5)).unwrap();
        bundle.notify_background_start(&id,"command",1.0);assert_eq!(bundle.background_snapshot()[0].id,id);bundle.park();bundle.notify_background_exit(&id);assert!(bundle.background_snapshot().is_empty());
        let (sender,receiver)=std::sync::mpsc::channel();let state_sender=sender.clone();bundle.bind_backgrounds(Arc::new(move |state| {state_sender.send(format!("state {}",state.len())).unwrap();}),Arc::new(move |id,runtime| {sender.send(format!("exit {id} {}",runtime.full_output().unwrap().trim())).unwrap();}));assert_eq!(receiver.try_recv().unwrap(),"state 0");assert_eq!(receiver.try_recv().unwrap(),format!("exit {id} ready"));assert!(receiver.try_recv().is_err());assert!(bundle.parked_exits.lock().unwrap().is_empty());bundle.teardown().unwrap();
    }
    #[test]
    fn claim_transfers_the_same_bundle_only_once() {
        let bundle=TerminalSessionBundle::new(1);let manager=bundle.manager.clone();park_bundle("bundle-claim-test",bundle).unwrap();
        let mut claimed=claim_parked_bundle("bundle-claim-test").expect("parked bundle");assert!(Arc::ptr_eq(&claimed.manager,&manager));assert!(claim_parked_bundle("bundle-claim-test").is_none());claimed.teardown().unwrap();
    }
    #[test]
    fn adopt_moves_live_state_between_generation_arcs() {
        let mut source=TerminalSessionBundle::new(1);let id=source.manager.lock().unwrap().create("ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();source.notify_background_start(&id,"command",1.0);
        let mut target=TerminalSessionBundle::new(4);target.adopt(&source);
        assert_eq!(target.manager.lock().unwrap().size(),1);assert_eq!(target.background_snapshot()[0].id,id);assert_eq!(source.manager.lock().unwrap().size(),0);assert!(source.background_snapshot().is_empty());
        target.teardown().unwrap();
    }
    #[tokio::test]
    async fn parked_monitor_completion_flushes_into_new_owner() {
        let mut bundle=TerminalSessionBundle::new(1);
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        bundle.monitors.lock().unwrap().register(&runtime,crate::monitor_registry::CommandMonitor::new(crate::monitor_registry::MonitorSnapshotEntry {id:"bash_1".to_owned(),..Default::default()},None)).unwrap();
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();bundle.bind(Arc::new(move |event| {sender.send(event).unwrap();}));
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(MonitorEvent::Line {..})));assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {..})));}).await.unwrap();bundle.teardown().unwrap();runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn parked_monitor_ending_flushes_to_new_owner_and_publishes_state() {
        let mut bundle=TerminalSessionBundle::new(1);
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        let (ended_sender,mut ended)=tokio::sync::mpsc::unbounded_channel();let (state_sender,mut states)=tokio::sync::mpsc::unbounded_channel();
        bundle.park();
        bundle.monitors.lock().unwrap().register(&runtime,crate::monitor_registry::CommandMonitor::new(crate::monitor_registry::MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"ready".to_owned(),started_at_ms:5.0,..Default::default()},None)).unwrap();
        let mut registry_state=bundle.monitors.lock().unwrap().subscribe_state();
        tokio::time::timeout(std::time::Duration::from_secs(5),async {loop {
            if registry_state.borrow_and_update().is_empty() {break;}
            if registry_state.changed().await.is_err() {break;}
        }}).await.unwrap();
        let sinks=TerminalEventSinks {on_monitor_event:Arc::new(|_|{}),on_monitor_state:Arc::new(move |_,transition| {state_sender.send(transition).unwrap();}),on_monitor_ended:Arc::new(move |event| {ended_sender.send(event).unwrap();}),on_background_state:Arc::new(|_|{}),on_background_exit:Arc::new(|_,_|{})};
        bundle.bind_sinks(sinks);
        let event=ended.try_recv().expect("flushed ending");assert_eq!(event.id,"bash_1");assert_eq!((event.reason,event.exit_code),(crate::monitor_registry::MonitorEndedReason::Exit,Some(0)));assert!(event.fire_count>=1);assert!(states.try_recv().is_err());
        bundle.publish_state(&bundle.monitors.lock().unwrap().snapshot(),false);assert!(!states.try_recv().unwrap());bundle.teardown().unwrap();runtime.dispose().unwrap();
    }
}