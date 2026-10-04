#[derive(Clone,Debug,Default,PartialEq)]
pub struct MonitorSnapshotEntry {
    pub id:String,
    pub monitor_id:Option<String>,
    pub description:String,
    pub paused:bool,
    pub started_at_ms:f64,
    pub command:Option<String>,
    pub filter:Option<String>,
    pub persistent:Option<bool>,
    pub deadline_ms:Option<f64>,
    pub fire_count:Option<usize>,
    pub last_fired_at_ms:Option<f64>,
    pub expires_at:Option<f64>,
    pub fire_window:Option<MonitorFireWindow>,
}

#[derive(Clone,Debug,PartialEq)]
pub struct MonitorFireWindow {pub start_ms:f64,pub count:usize}

#[derive(Clone,Debug,PartialEq,Eq)]
pub enum MonitorEvent {Line {id:String,description:String,line:String},Summary {id:String,description:String,summary:String}}

#[derive(Clone,Debug,PartialEq,Eq)]
pub enum MonitorEndedReason {Exit,Timeout,Killed,Disposed}
impl MonitorEndedReason {pub const fn as_str(&self)->&'static str {match self {Self::Exit=>"exit",Self::Timeout=>"timeout",Self::Killed=>"killed",Self::Disposed=>"disposed"}}}
#[derive(Clone,Debug,PartialEq)]
pub struct MonitorEndedEvent {pub id:String,pub description:String,pub started_at_ms:f64,pub ended_at_ms:f64,pub reason:MonitorEndedReason,pub exit_code:Option<i32>,pub fire_count:usize}
type MonitorEndedSink=std::sync::Arc<dyn Fn(MonitorEndedEvent)+Send+Sync>;
fn emit_ended(sink:&MonitorEndedSink,snapshot:&MonitorSnapshotEntry,reason:MonitorEndedReason,exit_code:Option<i32>,now:f64) {sink(MonitorEndedEvent {id:snapshot.id.clone(),description:snapshot.description.clone(),started_at_ms:snapshot.started_at_ms,ended_at_ms:now,reason,exit_code,fire_count:snapshot.fire_count.unwrap_or(0)});}
fn file_settle_reason(summary:&str)->MonitorEndedReason {match summary {"watcher timed_out"=>MonitorEndedReason::Timeout,"watcher killed"=>MonitorEndedReason::Killed,"watcher disposed"=>MonitorEndedReason::Disposed,_=>MonitorEndedReason::Exit}}

pub fn allocate_monitor_id()->std::io::Result<String> {
    use std::io::Read;
    let mut bytes=[0u8;10];std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(monitor_id_from_bytes(bytes))
}
fn monitor_id_from_bytes(bytes:[u8;10])->String {
    let mut id=String::from("mon_");let mut buffer=0u32;let mut bits=0;
    for byte in bytes {buffer=(buffer<<8)|u32::from(byte);bits+=8;while bits>=5 {bits-=5;id.push(b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[((buffer>>bits)&31) as usize] as char);}}
    id
}

pub struct CommandMonitor {
    pub snapshot:MonitorSnapshotEntry,
    pub muted_dropped:usize,
    filter:Option<fancy_regex::Regex>,
    lines:crate::monitor_line_buffer::MonitorLineBuffer,
    settled:bool,
}
impl CommandMonitor {
    pub fn new(snapshot:MonitorSnapshotEntry,filter:Option<fancy_regex::Regex>)->Self {
        let mut snapshot=snapshot;snapshot.fire_count=Some(0);
        Self {snapshot,muted_dropped:0,filter,lines:Default::default(),settled:false}
    }
    fn record_fire(&mut self,now:f64) {
        self.snapshot.fire_count=Some(self.snapshot.fire_count.unwrap_or(0)+1);
        self.snapshot.last_fired_at_ms=Some(now);
    }
    pub fn consume(&mut self,chunk:&str,now:f64)->Vec<MonitorEvent> {
        if self.settled||chunk.is_empty() {return vec![];}
        let mut events=vec![];
        for line in self.lines.append(chunk) {
            if self.filter.as_ref().is_some_and(|filter|!filter.is_match(&line).unwrap_or(false)) {continue;}
            if self.snapshot.paused {self.muted_dropped+=1;continue;}
            self.record_fire(now);
            events.push(MonitorEvent::Line {id:self.snapshot.id.clone(),description:self.snapshot.description.clone(),line});
            if let Some(window)=&mut self.snapshot.fire_window {
                if now-window.start_ms>=crate::shared::FIRE_BUDGET_WINDOW_MS as f64 {*window=MonitorFireWindow {start_ms:now,count:0};}
                window.count+=1;
                if window.count>=crate::shared::DEFAULT_DURABLE_MONITOR_FIRE_BUDGET {
                    self.snapshot.paused=true;
                    self.record_fire(now);
                    events.push(MonitorEvent::Summary {id:self.snapshot.id.clone(),description:self.snapshot.description.clone(),summary:crate::shared::FIRE_BUDGET_AUTO_MUTE_SUMMARY.to_owned()});
                }
            }
        }
        events
    }
    pub fn pause(&mut self)->bool {if self.snapshot.paused {return false;}self.snapshot.paused=true;true}
    pub fn resume(&mut self)->Option<usize> {
        if !self.snapshot.paused {return None;}
        self.snapshot.paused=false;
        if let Some(window)=&mut self.snapshot.fire_window {window.count=0;}
        Some(std::mem::take(&mut self.muted_dropped))
    }
    pub fn settle(&mut self,summary:String,now:f64)->Option<MonitorEvent> {
        if self.settled {return None;}
        self.settled=true;self.record_fire(now);
        Some(MonitorEvent::Summary {id:self.snapshot.id.clone(),description:self.snapshot.description.clone(),summary})
    }
    pub fn dispose(&mut self) {self.settled=true;}
}

pub struct MonitorRegistry {
    records:std::sync::Arc<std::sync::Mutex<indexmap::IndexMap<String,CommandMonitor>>>,
    tasks:Vec<tokio::task::JoinHandle<()>>,
    emit:std::sync::Arc<dyn Fn(MonitorEvent)+Send+Sync>,
    files:indexmap::IndexMap<String,(std::sync::Arc<std::sync::Mutex<crate::file_monitor::FileMonitor>>,tokio::task::JoinHandle<()>)>,
    file_snapshots:std::sync::Arc<std::sync::Mutex<indexmap::IndexMap<String,MonitorSnapshotEntry>>>,
    next_file_id:usize,
    transitions:tokio::sync::watch::Sender<Vec<MonitorSnapshotEntry>>,
    parked:tokio::sync::watch::Sender<bool>,
    delivery:std::sync::Arc<std::sync::Mutex<MonitorRouting>>,
    ended:MonitorEndedSink,
    disposed:bool,
}
type MonitorSink=std::sync::Arc<dyn Fn(MonitorEvent)+Send+Sync>;
#[derive(Default)]
struct MonitorRouting {sink:Option<MonitorSink>,pending:std::collections::VecDeque<MonitorEvent>}
fn publish_snapshot(records:&std::sync::Mutex<indexmap::IndexMap<String,CommandMonitor>>,files:&std::sync::Mutex<indexmap::IndexMap<String,MonitorSnapshotEntry>>,sender:&tokio::sync::watch::Sender<Vec<MonitorSnapshotEntry>>) {
    let mut snapshot=records.lock().expect("monitor records").values().map(|record|record.snapshot.clone()).collect::<Vec<_>>();snapshot.extend(files.lock().expect("file snapshots").values().cloned());
    sender.send_if_modified(|current| {if *current==snapshot {false} else {*current=snapshot;true}});
}
fn now_ms()->f64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0}
impl MonitorRegistry {
    pub fn new(emit:impl Fn(MonitorEvent)+Send+Sync+'static)->Self {Self::new_with_ended(emit,|_|{})}
    pub fn new_with_ended(emit:impl Fn(MonitorEvent)+Send+Sync+'static,ended:impl Fn(MonitorEndedEvent)+Send+Sync+'static)->Self {
        let delivery=std::sync::Arc::new(std::sync::Mutex::new(MonitorRouting {sink:Some(std::sync::Arc::new(emit)),..Default::default()}));let routing=delivery.clone();
        let emit=std::sync::Arc::new(move |event:MonitorEvent| {let sink={let mut routing=routing.lock().expect("monitor routing");if let Some(sink)=&routing.sink {Some(sink.clone())}else {routing.pending.push_back(event.clone());if routing.pending.len()>100 {routing.pending.pop_front();}None}};if let Some(sink)=sink {sink(event);}});
        Self {records:Default::default(),tasks:vec![],emit,files:Default::default(),file_snapshots:Default::default(),next_file_id:0,transitions:tokio::sync::watch::channel(vec![]).0,parked:tokio::sync::watch::channel(false).0,delivery,ended:std::sync::Arc::new(ended),disposed:false}
    }
    pub fn detach_delivery(&self) {self.delivery.lock().expect("monitor routing").sink=None;}
    pub fn bind_delivery(&self,emit:impl Fn(MonitorEvent)+Send+Sync+'static) {
        let sink:MonitorSink=std::sync::Arc::new(emit);let pending={let mut routing=self.delivery.lock().expect("monitor routing");routing.sink=Some(sink.clone());std::mem::take(&mut routing.pending)};for event in pending {sink(event);}
    }
    pub fn park(&self) {self.parked.send_replace(true);}
    pub fn unpark(&self) {self.parked.send_replace(false);}
    pub fn subscribe_state(&self)->tokio::sync::watch::Receiver<Vec<MonitorSnapshotEntry>> {self.transitions.subscribe()}
    pub fn muted_dropped(&self,id:&str)->usize {self.records.lock().expect("monitor records").get(id).map_or(0,|record|record.muted_dropped)}
    fn publish_state(&self) {publish_snapshot(&self.records,&self.file_snapshots,&self.transitions);}
    #[cfg(unix)]
    pub fn register_file(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent,timeout_ms:u64)->std::io::Result<(String,String)> {
        self.register_file_with_identity(description,path,event,timeout_ms,None,None)
    }
    #[cfg(unix)]
    pub fn register_file_with_identity(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent,timeout_ms:u64,monitor_id:Option<&str>,approved_parent:Option<&std::path::Path>)->std::io::Result<(String,String)> {
        self.register_file_lifetime(description,path,event,(timeout_ms,false),monitor_id,approved_parent)
    }
    #[cfg(unix)]
    pub fn register_persistent_file(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent)->std::io::Result<(String,String)> {
        self.register_persistent_file_with_identity(description,path,event,None)
    }
    #[cfg(unix)]
    pub fn register_persistent_file_with_identity(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent,approved_parent:Option<&std::path::Path>)->std::io::Result<(String,String)> {
        self.register_file_lifetime(description,path,event,(crate::shared::DURABLE_MONITOR_EXPIRY_MS,true),None,approved_parent)
    }
    #[cfg(unix)]
    pub fn restore_persistent_file(&mut self,monitor:&crate::terminal_manifest_model::ManifestMonitor,path:&std::path::Path,now:f64)->std::io::Result<(String,String)> {
        let registered=self.register_file_lifetime(&monitor.description,path,monitor.event.unwrap_or(crate::terminal_manifest_model::FileEvent::Create),(crate::durable_file::remaining_ms(monitor,now) as u64,true),Some(&monitor.monitor_id),monitor.approved_parent.as_deref().map(std::path::Path::new))?;
        if let Some(snapshot)=self.file_snapshots.lock().expect("file snapshots").get_mut(&registered.0) {snapshot.expires_at=monitor.expires_at;}
        self.publish_state();
        Ok(registered)
    }
    #[cfg(unix)]
    fn register_file_lifetime(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent,lifetime:(u64,bool),monitor_id:Option<&str>,approved_parent:Option<&std::path::Path>)->std::io::Result<(String,String)> {
        if self.disposed {return Err(std::io::Error::other("Cannot create file monitor: monitor registry is disposed."));}
        let (timeout_ms,persistent)=lifetime;
        let id=format!("watch_{}",self.next_file_id+1);let monitor_id=match monitor_id {Some(id)=>id.to_owned(),None=>allocate_monitor_id()?};
        let file=std::sync::Arc::new(std::sync::Mutex::new(crate::file_monitor::FileMonitor::register(id.clone(),description.to_owned(),path,event,approved_parent)?));
        let checker=file.clone();let emit=self.emit.clone();let snapshots=self.file_snapshots.clone();let runtime_id=id.clone();let ended=self.ended.clone();
        snapshots.lock().expect("file snapshots").insert(id.clone(),MonitorSnapshotEntry {id:id.clone(),monitor_id:Some(monitor_id.clone()),description:description.to_owned(),started_at_ms:now_ms(),persistent:Some(persistent),deadline_ms:(!persistent).then_some(now_ms()+timeout_ms as f64),expires_at:persistent.then_some(now_ms()+timeout_ms as f64),..Default::default()});
        snapshots.lock().expect("file snapshots").get_mut(&id).expect("registered file snapshot").fire_count=Some(0);
        self.publish_state();let records=self.records.clone();let transitions=self.transitions.clone();
        let mut state=self.subscribe_state();
        let mut parked=self.parked.subscribe();
        let expires=tokio::time::Instant::now()+std::time::Duration::from_millis(timeout_ms);
        let watch=tokio::spawn(async move {
            let period=std::time::Duration::from_millis(crate::monitor_file_watch::FILE_MONITOR_POLL_MS);
            let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let deadline=tokio::time::sleep_until(expires);tokio::pin!(deadline);
            loop {
                let paused=checker.lock().expect("file monitor").paused||*parked.borrow();
                let timed_out=tokio::select! {
                    _=timer.tick(),if !paused=>false,
                    _=&mut deadline=>true,
                    changed=state.changed()=>{if changed.is_err() {return;}state.borrow_and_update();timer.reset();continue;}
                    changed=parked.changed()=>{if changed.is_err() {return;}let resumed=!*parked.borrow_and_update();if resumed {timer.reset_immediately();}else {timer.reset();}continue;}
                };
                let mut summary=None;
                let (events,settled)={let mut file=checker.lock().expect("file monitor");let events=if timed_out {summary=Some((if persistent {"watcher expired"} else {"watcher timed_out"}).to_owned());file.stop(summary.as_deref().unwrap_or("watcher timed_out")).into_iter().collect()} else {match file.check() {Ok(events)=>events,Err(error)=>{summary=Some(format!("watcher error: {error}"));file.stop(summary.as_deref().unwrap_or("watcher error")).into_iter().collect()}}};(events,file.settled)};
                if settled {let snapshot=snapshots.lock().expect("file snapshots").shift_remove(&runtime_id);if let Some(snapshot)=snapshot {emit_ended(&ended,&snapshot,file_settle_reason(summary.as_deref().unwrap_or("")),None,now_ms());}}
                publish_snapshot(&records,&snapshots,&transitions);
                for event in events {emit(event);}
                if settled {return;}
            }
        });
        self.files.retain(|_,(_,task)|!task.is_finished());
        self.next_file_id+=1;self.files.insert(id.clone(),(file,watch));Ok((id,monitor_id))
    }
    #[cfg(unix)]
    pub fn stop_file(&mut self,id:&str)->bool {
        let Some((file,watch))=self.files.shift_remove(id) else {return false;};watch.abort();let snapshot=self.file_snapshots.lock().expect("file snapshots").shift_remove(id);
        let event=file.lock().expect("file monitor").stop("watcher killed");self.publish_state();if let Some(event)=event {(self.emit)(event);}if let Some(snapshot)=snapshot {emit_ended(&self.ended,&snapshot,MonitorEndedReason::Killed,None,now_ms());}true
    }
    pub fn file_checkpoint(&self,id:&str)->Option<crate::terminal_manifest_model::TerminalManifestCheckpoint> {
        let (file,_)=self.files.get(id)?;let file=file.lock().expect("file monitor");if file.settled {None} else {Some(file.checkpoint.clone())}
    }
    pub fn reserve_file_capacity(&self,id:&str,reservation:crate::manager::CapacityReservation) {if let Some((file,_))=self.files.get(id) {file.lock().expect("file monitor").reserve_capacity(reservation);}}
    pub fn adopt_fire_window(&self,monitor_id:&str,window:&crate::terminal_manifest_model::ManifestFireWindow) {
        let window=MonitorFireWindow {start_ms:window.start_ms,count:window.count as usize};
        for record in self.records.lock().expect("monitor records").values_mut() {if record.snapshot.monitor_id.as_deref()==Some(monitor_id) {record.snapshot.fire_window=Some(window.clone());}}
        for record in self.file_snapshots.lock().expect("file snapshots").values_mut() {if record.monitor_id.as_deref()==Some(monitor_id) {record.fire_window=Some(window.clone());}}
        self.publish_state();
    }
    pub fn emit_file_line(&self,id:&str,line:String)->bool {
        let Some((file,_))=self.files.get(id) else {return false;};
        let event={let file=file.lock().expect("file monitor");if file.settled {return false;}MonitorEvent::Line {id:file.id.clone(),description:file.description.clone(),line}};
        if let Some(snapshot)=self.file_snapshots.lock().expect("file snapshots").get_mut(id) {snapshot.fire_count=Some(snapshot.fire_count.unwrap_or(0)+1);snapshot.last_fired_at_ms=Some(now_ms());}
        self.publish_state();(self.emit)(event);true
    }
    pub fn snapshot(&self)->Vec<MonitorSnapshotEntry> {let mut snapshot=self.records.lock().expect("monitor records").values().map(|record|record.snapshot.clone()).collect::<Vec<_>>();snapshot.extend(self.file_snapshots.lock().expect("file snapshots").values().cloned());snapshot}
    pub fn pause(&self,ids:&[String])->Vec<String> {
        let mut records=self.records.lock().expect("monitor records");
        let mut paused=ids.iter().filter(|id|records.get_mut(*id).is_some_and(CommandMonitor::pause)).cloned().collect::<Vec<_>>();
        for id in ids {if let Some((file,_))=self.files.get(id) {let mut file=file.lock().expect("file monitor");if !file.paused&&!file.settled {file.paused=true;paused.push(id.clone());if let Some(snapshot)=self.file_snapshots.lock().expect("file snapshots").get_mut(id) {snapshot.paused=true;}}}}
        drop(records);self.publish_state();paused
    }
    pub fn resume(&self,ids:Option<&[String]>)->Vec<(String,usize)> {
        let mut records=self.records.lock().expect("monitor records");
        let ids=ids.map(<[String]>::to_vec).unwrap_or_else(||records.keys().chain(self.files.keys()).cloned().collect());
        let mut resumed=vec![];
        let mut pending_events=vec![];
        for id in ids {
            if let Some(dropped)=records.get_mut(&id).and_then(CommandMonitor::resume) {resumed.push((id,dropped));continue;}
            if let Some((file,_))=self.files.get(&id) {
                let (events,settled)={let mut file=file.lock().expect("file monitor");if !file.paused||file.settled {continue;}file.paused=false;let events=file.check().unwrap_or_else(|error|file.stop(&format!("watcher error: {error}")).into_iter().collect());(events,file.settled)};
                let mut snapshots=self.file_snapshots.lock().expect("file snapshots");
                if settled {snapshots.shift_remove(&id);} else if let Some(snapshot)=snapshots.get_mut(&id) {snapshot.paused=false;if let Some(window)=&mut snapshot.fire_window {window.count=0;}}
                resumed.push((id,0));pending_events.extend(events);
            }
        }
        drop(records);
        self.publish_state();
        for event in pending_events {(self.emit)(event);}
        resumed
    }
    pub fn register(&mut self,runtime:&crate::runtime_session::TerminalRuntimeSession,mut record:CommandMonitor)->Result<(),crate::runtime_session::RuntimeError> {
        if self.disposed {return Err(crate::runtime_session::RuntimeError::RegistryDisposed);}
        let (history,mut output)=runtime.subscribe_output()?;
        let mut exit=runtime.subscribe_exit();
        record.snapshot.started_at_ms=now_ms();
        let id=record.snapshot.id.clone();
        let initial=record.consume(&history,now_ms());
        self.records.lock().expect("monitor records").insert(id.clone(),record);
        self.publish_state();
        for event in initial {(self.emit)(event);}
        let records=self.records.clone();let emit=self.emit.clone();let files=self.file_snapshots.clone();let transitions=self.transitions.clone();let ended=self.ended.clone();
        self.tasks.retain(|task|!task.is_finished());
        self.tasks.push(tokio::spawn(async move {
            loop {
                let settled=exit.borrow().clone();
                if let Some(result)=settled {
                    let Some(mut record)=records.lock().expect("monitor records").shift_remove(&id) else {return;};
                    publish_snapshot(&records,&files,&transitions);
                    while let Ok(chunk)=output.try_recv() {for event in record.consume(&chunk,now_ms()) {emit(event);}}
                    let (reason,exit_code)=match &result {Ok(exit)=>(if exit.timed_out {MonitorEndedReason::Timeout} else if exit.cancelled {MonitorEndedReason::Killed} else {MonitorEndedReason::Exit},exit.exit_code),Err(_)=>(MonitorEndedReason::Exit,None)};
                    let summary=match result {Ok(result)=>format!("watcher {}{}",crate::tools::spawn::describe_exit(Some(&result)).unwrap_or_else(||"exited".to_owned()),result.exit_code.map_or(String::new(),|code|format!(" (exit code {code})"))),Err(error)=>format!("watcher error: {error}")};
                    if let Some(event)=record.settle(summary,now_ms()) {emit(event);}
                    emit_ended(&ended,&record.snapshot,reason,exit_code,now_ms());
                    return;
                }
                tokio::select! {
                    chunk=output.recv()=>{let Some(chunk)=chunk else {return;};let events=records.lock().expect("monitor records").get_mut(&id).map(|record|record.consume(&chunk,now_ms())).unwrap_or_default();publish_snapshot(&records,&files,&transitions);for event in events {emit(event);}},
                    changed=exit.changed()=>{if changed.is_err() {return;}}
                }
            }
        }));
        Ok(())
    }
    pub fn dispose(&mut self) {if self.disposed {return;}self.disposed=true;let now=now_ms();let mut events=vec![];for (_,(file,task)) in std::mem::take(&mut self.files) {task.abort();if let Some(event)=file.lock().expect("file monitor").stop("watcher disposed") {events.push(event);}}for snapshot in self.file_snapshots.lock().expect("file snapshots").values() {emit_ended(&self.ended,snapshot,MonitorEndedReason::Disposed,None,now);}self.file_snapshots.lock().expect("file snapshots").clear();for task in self.tasks.drain(..) {task.abort();}for record in self.records.lock().expect("monitor records").values() {emit_ended(&self.ended,&record.snapshot,MonitorEndedReason::Disposed,None,now);}self.records.lock().expect("monitor records").clear();self.publish_state();for event in events {(self.emit)(event);}self.delivery.lock().expect("monitor routing").pending.clear();}
}
impl Drop for MonitorRegistry {fn drop(&mut self) {self.dispose();}}

#[cfg(test)]
mod registry_tests {
    use super::*;
    #[tokio::test]
    async fn disposed_registry_rejects_new_file_and_command_watches() {
        let dir=tempfile::tempdir().unwrap();let mut registry=MonitorRegistry::new(|_|{});registry.dispose();registry.dispose();
        assert_eq!(registry.register_file("file",&dir.path().join("file"),crate::terminal_manifest_model::FileEvent::Create,1000).unwrap_err().to_string(),"Cannot create file monitor: monitor registry is disposed.");
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("read",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("read value")).unwrap();
        assert!(matches!(registry.register(&runtime,CommandMonitor::new(MonitorSnapshotEntry::default(),None)),Err(crate::runtime_session::RuntimeError::RegistryDisposed)));assert!(registry.snapshot().is_empty());runtime.dispose().unwrap();
    }
    #[test]
    fn detached_delivery_bounds_and_rebinds_events_in_order() {
        let registry=MonitorRegistry::new(|_|panic!("old owner received parked event"));registry.detach_delivery();
        for index in 0..105 {(registry.emit)(MonitorEvent::Line {id:"bash_1".to_owned(),description:"watch".to_owned(),line:index.to_string()});}
        let (sender,receiver)=std::sync::mpsc::channel();registry.bind_delivery(move |event| {sender.send(event).unwrap();});
        let events=receiver.try_iter().collect::<Vec<_>>();assert_eq!(events.len(),100);assert!(matches!(&events[0],MonitorEvent::Line {line,..} if line=="5"));assert!(matches!(&events[99],MonitorEvent::Line {line,..} if line=="104"));
    }
    #[tokio::test]
    async fn snapshot_preserves_registration_order_beyond_nine_file_ids() {
        let dir=tempfile::tempdir().unwrap();let mut registry=MonitorRegistry::new(|_|{});let mut ids=vec![];
        for index in 0..12 {let (id,_)=registry.register_persistent_file(&format!("watch {index}"),&dir.path().join(format!("file{index}")),crate::terminal_manifest_model::FileEvent::Create).unwrap();ids.push(id);}
        assert_eq!(registry.snapshot().iter().map(|entry|entry.id.clone()).collect::<Vec<_>>(),ids);
        registry.stop_file(&ids[3]);ids.remove(3);assert_eq!(registry.snapshot().iter().map(|entry|entry.id.clone()).collect::<Vec<_>>(),ids);registry.dispose();
    }
    #[tokio::test]
    async fn file_disposal_emits_summary_once_after_releasing_capacity() {
        let dir=tempfile::tempdir().unwrap();let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut manager=crate::manager::TerminalManager::default();
        let (id,_)=registry.register_persistent_file("watch",&dir.path().join("file"),crate::terminal_manifest_model::FileEvent::Create).unwrap();registry.reserve_file_capacity(&id,manager.reserve().unwrap().unwrap());registry.dispose();
        assert_eq!(manager.active_size().unwrap(),0);assert!(registry.snapshot().is_empty());assert!(matches!(events.try_recv(),Ok(MonitorEvent::Summary {summary,..}) if summary=="watcher disposed"));registry.dispose();assert!(events.try_recv().is_err());
    }
    #[tokio::test(start_paused=true)]
    async fn paused_file_preserves_checkpoint_and_resume_checks_immediately() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("file");std::fs::write(&path,b"old").unwrap();let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});
        let (id,_)=registry.register_persistent_file("watch",&path,crate::terminal_manifest_model::FileEvent::Modify).unwrap();let saved=registry.file_checkpoint(&id).unwrap();registry.pause(std::slice::from_ref(&id));std::fs::write(&path,b"new content").unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;assert_eq!(registry.file_checkpoint(&id),Some(saved));assert!(events.try_recv().is_err());
        registry.resume(Some(std::slice::from_ref(&id)));assert!(matches!(events.try_recv(),Ok(MonitorEvent::Line {..})));assert!(matches!(events.try_recv(),Ok(MonitorEvent::Summary {..})));assert!(registry.snapshot().is_empty());registry.dispose();
    }
    #[tokio::test]
    async fn state_subscription_observes_registration_pause_rearm_and_stop() {
        let dir=tempfile::tempdir().unwrap();let mut registry=MonitorRegistry::new(|_|{});let mut state=registry.subscribe_state();assert!(state.borrow_and_update().is_empty());
        let (id,_)=registry.register_persistent_file("watch",&dir.path().join("file"),crate::terminal_manifest_model::FileEvent::Create).unwrap();state.changed().await.unwrap();assert_eq!(state.borrow_and_update()[0].id,id);
        registry.pause(std::slice::from_ref(&id));state.changed().await.unwrap();assert!(state.borrow_and_update()[0].paused);
        registry.resume(Some(std::slice::from_ref(&id)));state.changed().await.unwrap();assert!(!state.borrow_and_update()[0].paused);
        assert!(registry.stop_file(&id));state.changed().await.unwrap();assert!(state.borrow_and_update().is_empty());
    }
    #[tokio::test(start_paused=true)]
    async fn file_timeout_starts_at_registration_not_task_first_poll() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("created");
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});
        registry.register_file("created",&path,crate::terminal_manifest_model::FileEvent::Create,1000).unwrap();
        tokio::time::advance(std::time::Duration::from_millis(1000)).await;
        assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {summary,..}) if summary=="watcher timed_out"));assert!(registry.snapshot().is_empty());
    }
    #[tokio::test(start_paused=true)]
    async fn parked_files_do_not_read_changes_and_unpark_preserves_explicit_mute() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("created");
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});
        let (active,_)=registry.register_persistent_file("active",&path,crate::terminal_manifest_model::FileEvent::Create).unwrap();
        let (muted,_)=registry.register_persistent_file("muted",&path,crate::terminal_manifest_model::FileEvent::Create).unwrap();
        registry.pause(std::slice::from_ref(&muted));registry.park();std::fs::write(&path,b"ready").unwrap();
        tokio::time::advance(std::time::Duration::from_millis(1000)).await;assert!(events.try_recv().is_err());assert_eq!(registry.snapshot().len(),2);
        registry.unpark();assert!(matches!(events.recv().await,Some(MonitorEvent::Line {id,..}) if id==active));assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {id,..}) if id==active));
        assert_eq!(registry.snapshot().len(),1);assert!(registry.snapshot()[0].paused);assert_eq!(registry.snapshot()[0].id,muted);registry.dispose();
    }
    #[tokio::test]
    async fn file_rearm_callbacks_can_read_command_records() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("created");
        let mut registry=MonitorRegistry::new(|_|{});let records=registry.records.clone();
        registry.emit=std::sync::Arc::new(move |_| {assert!(records.try_lock().is_ok());});
        let (id,_)=registry.register_file("created",&path,crate::terminal_manifest_model::FileEvent::Create,5000).unwrap();
        registry.pause(std::slice::from_ref(&id));std::fs::write(&path,b"ready").unwrap();
        assert_eq!(registry.resume(Some(std::slice::from_ref(&id))),vec![(id,0)]);
        assert!(registry.snapshot().is_empty());
    }
    #[tokio::test]
    async fn native_file_registration_emits_once_and_releases_live_snapshot() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("created");
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});
        let (id,_)=registry.register_file("created",&path,crate::terminal_manifest_model::FileEvent::Create,5000).unwrap();
        assert_eq!(registry.snapshot().len(),1);std::fs::write(&path,b"ready").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(MonitorEvent::Line {..})));assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {summary,..}) if summary=="watcher completed"));}).await.unwrap();
        assert!(registry.snapshot().is_empty());registry.stop_file(&id);assert!(events.try_recv().is_err());
    }
    #[tokio::test]
    async fn native_watch_emits_lines_then_exactly_one_completion() {
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();
        let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("printf ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();
        registry.register(&runtime,CommandMonitor::new(MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"ready".to_owned(),..Default::default()},None)).unwrap();
        let observed=tokio::time::timeout(std::time::Duration::from_secs(5),async {let line=events.recv().await.unwrap();let summary=events.recv().await.unwrap();(line,summary)}).await.unwrap();
        assert!(matches!(observed.0,MonitorEvent::Line {line,..} if line=="ready"));
        assert!(matches!(observed.1,MonitorEvent::Summary {summary,..} if summary=="watcher completed (exit code 0)"));
        assert!(registry.snapshot().is_empty());assert!(events.try_recv().is_err());runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn command_completion_emits_one_ended_event_with_exit_reason() {
        let (ended_sender,mut ended)=tokio::sync::mpsc::unbounded_channel();
        let mut registry=MonitorRegistry::new_with_ended(|_|{},move |event| {ended_sender.send(event).unwrap();});
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("printf ready",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'")).unwrap();
        registry.register(&runtime,CommandMonitor::new(MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"ready".to_owned(),..Default::default()},None)).unwrap();
        let event=tokio::time::timeout(std::time::Duration::from_secs(5),ended.recv()).await.unwrap().unwrap();
        assert_eq!((event.id.as_str(),event.description.as_str(),event.reason,event.exit_code),("bash_1","ready",MonitorEndedReason::Exit,Some(0)));assert!(event.fire_count>=1);assert!(ended.try_recv().is_err());registry.dispose();runtime.dispose().unwrap();
    }
    #[test]
    fn dispose_emits_disposed_ending_for_every_live_record() {
        let (ended_sender,ended)=std::sync::mpsc::channel();
        let mut registry=MonitorRegistry::new_with_ended(|_|{},move |event| {ended_sender.send(event).unwrap();});
        let runtime=crate::runtime_session::TerminalRuntimeSession::start("read value",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("read value")).unwrap();
        registry.register(&runtime,CommandMonitor::new(MonitorSnapshotEntry {id:"bash_1".to_owned(),..Default::default()},None)).unwrap();
        let dir=tempfile::tempdir().unwrap();let (file_id,_)=registry.register_persistent_file("watch",&dir.path().join("file"),crate::terminal_manifest_model::FileEvent::Create).unwrap();
        registry.dispose();
        let events=ended.try_iter().collect::<Vec<_>>();assert_eq!(events.iter().filter(|event|event.reason==MonitorEndedReason::Disposed).count(),2);assert!(events.iter().any(|event|event.id=="bash_1"));assert!(events.iter().any(|event|event.id==file_id));
        runtime.dispose().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn stable_identity_encodes_eighty_random_bits() {assert_eq!(monitor_id_from_bytes([0;10]),"mon_0000000000000000");assert_eq!(monitor_id_from_bytes([255;10]),"mon_ZZZZZZZZZZZZZZZZ");assert_eq!(allocate_monitor_id().unwrap().len(),20);}
    fn monitor()->CommandMonitor {CommandMonitor::new(MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"ready".to_owned(),fire_window:Some(MonitorFireWindow {start_ms:10.0,count:0}),..Default::default()},Some(fancy_regex::Regex::new("^ready").unwrap()))}
    #[test]
    fn filtering_buffers_partial_lines_and_counts_only_muted_matches() {
        let mut monitor=monitor();assert!(monitor.consume("ignored\nrea",10.0).is_empty());
        assert_eq!(monitor.consume("dy\n",11.0).len(),1);
        assert!(monitor.pause());assert!(monitor.consume("ignored\nready again\n",12.0).is_empty());
        assert_eq!(monitor.resume(),Some(1));assert_eq!(monitor.resume(),None);
        assert_eq!(monitor.snapshot.fire_window.as_ref().unwrap().count,0);
        assert!(monitor.settle("watcher exited (exit code 0)".to_owned(),13.0).is_some());
        assert!(monitor.settle("duplicate".to_owned(),14.0).is_none());assert!(monitor.consume("ready\n",15.0).is_empty());
    }
    #[test]
    fn durable_budget_mutes_at_limit_and_rearm_preserves_window_start() {
        let mut monitor=monitor();let events=monitor.consume(&"ready\n".repeat(202),20.0);
        assert_eq!(events.len(),201);assert!(matches!(events.last(),Some(MonitorEvent::Summary {summary,..}) if summary==crate::shared::FIRE_BUDGET_AUTO_MUTE_SUMMARY));
        assert_eq!(monitor.snapshot.fire_count,Some(201));assert_eq!(monitor.resume(),Some(2));
        assert_eq!(monitor.snapshot.fire_window.as_ref().unwrap().start_ms,10.0);
        monitor.consume("ready\n",crate::shared::FIRE_BUDGET_WINDOW_MS as f64+10.0);
        assert_eq!(monitor.snapshot.fire_window.as_ref().unwrap().count,1);
    }
}
