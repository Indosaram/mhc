#[derive(Clone,Debug,Default)]
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

#[derive(Clone,Debug)]
pub struct MonitorFireWindow {pub start_ms:f64,pub count:usize}

#[derive(Clone,Debug,PartialEq,Eq)]
pub enum MonitorEvent {Line {id:String,description:String,line:String},Summary {id:String,description:String,summary:String}}

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
    records:std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String,CommandMonitor>>>,
    tasks:Vec<tokio::task::JoinHandle<()>>,
    emit:std::sync::Arc<dyn Fn(MonitorEvent)+Send+Sync>,
    files:std::collections::BTreeMap<String,(std::sync::Arc<std::sync::Mutex<crate::file_monitor::FileMonitor>>,tokio::task::JoinHandle<()>)>,
    file_snapshots:std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String,MonitorSnapshotEntry>>>,
    next_file_id:usize,
}
fn now_ms()->f64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0}
impl MonitorRegistry {
    pub fn new(emit:impl Fn(MonitorEvent)+Send+Sync+'static)->Self {Self {records:Default::default(),tasks:vec![],emit:std::sync::Arc::new(emit),files:Default::default(),file_snapshots:Default::default(),next_file_id:0}}
    #[cfg(unix)]
    pub fn register_file(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent,timeout_ms:u64)->std::io::Result<(String,String)> {
        self.register_file_with_identity(description,path,event,timeout_ms,None,None)
    }
    #[cfg(unix)]
    pub fn register_file_with_identity(&mut self,description:&str,path:&std::path::Path,event:crate::terminal_manifest_model::FileEvent,timeout_ms:u64,monitor_id:Option<&str>,approved_parent:Option<&std::path::Path>)->std::io::Result<(String,String)> {
        let id=format!("watch_{}",self.next_file_id+1);let monitor_id=match monitor_id {Some(id)=>id.to_owned(),None=>allocate_monitor_id()?};
        let file=std::sync::Arc::new(std::sync::Mutex::new(crate::file_monitor::FileMonitor::register(id.clone(),description.to_owned(),path,event,approved_parent)?));
        let checker=file.clone();let emit=self.emit.clone();let snapshots=self.file_snapshots.clone();let runtime_id=id.clone();
        snapshots.lock().expect("file snapshots").insert(id.clone(),MonitorSnapshotEntry {id:id.clone(),monitor_id:Some(monitor_id.clone()),description:description.to_owned(),started_at_ms:now_ms(),deadline_ms:Some(now_ms()+timeout_ms as f64),..Default::default()});
        let watch=tokio::spawn(async move {
            let period=std::time::Duration::from_millis(crate::monitor_file_watch::FILE_MONITOR_POLL_MS);
            let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let deadline=tokio::time::sleep(std::time::Duration::from_millis(timeout_ms));tokio::pin!(deadline);
            loop {
                let timed_out=tokio::select! {_=timer.tick()=>false,_=&mut deadline=>true};
                let (events,settled)={let mut file=checker.lock().expect("file monitor");let events=if timed_out {file.stop("watcher timed_out").into_iter().collect()} else {match file.check() {Ok(events)=>events,Err(error)=>file.stop(&format!("watcher error: {error}")).into_iter().collect()}};(events,file.settled)};
                if settled {snapshots.lock().expect("file snapshots").remove(&runtime_id);}
                for event in events {emit(event);}
                if settled {return;}
            }
        });
        self.files.retain(|_,(_,task)|!task.is_finished());
        self.next_file_id+=1;self.files.insert(id.clone(),(file,watch));Ok((id,monitor_id))
    }
    #[cfg(unix)]
    pub fn stop_file(&mut self,id:&str)->bool {
        let Some((file,watch))=self.files.remove(id) else {return false;};watch.abort();self.file_snapshots.lock().expect("file snapshots").remove(id);
        let event=file.lock().expect("file monitor").stop("watcher killed");if let Some(event)=event {(self.emit)(event);}true
    }
    pub fn file_checkpoint(&self,id:&str)->Option<crate::terminal_manifest_model::TerminalManifestCheckpoint> {
        let (file,_)=self.files.get(id)?;let file=file.lock().expect("file monitor");if file.settled {None} else {Some(file.checkpoint.clone())}
    }
    pub fn reserve_file_capacity(&self,id:&str,reservation:crate::manager::CapacityReservation) {if let Some((file,_))=self.files.get(id) {file.lock().expect("file monitor").reserve_capacity(reservation);}}
    pub fn emit_file_line(&self,id:&str,line:String)->bool {
        let Some((file,_))=self.files.get(id) else {return false;};
        let event={let file=file.lock().expect("file monitor");if file.settled {return false;}MonitorEvent::Line {id:file.id.clone(),description:file.description.clone(),line}};
        (self.emit)(event);true
    }
    pub fn snapshot(&self)->Vec<MonitorSnapshotEntry> {let mut snapshot=self.records.lock().expect("monitor records").values().map(|record|record.snapshot.clone()).collect::<Vec<_>>();snapshot.extend(self.file_snapshots.lock().expect("file snapshots").values().cloned());snapshot}
    pub fn pause(&self,ids:&[String])->Vec<String> {
        let mut records=self.records.lock().expect("monitor records");
        let mut paused=ids.iter().filter(|id|records.get_mut(*id).is_some_and(CommandMonitor::pause)).cloned().collect::<Vec<_>>();
        for id in ids {if let Some((file,_))=self.files.get(id) {let mut file=file.lock().expect("file monitor");if !file.paused&&!file.settled {file.paused=true;paused.push(id.clone());if let Some(snapshot)=self.file_snapshots.lock().expect("file snapshots").get_mut(id) {snapshot.paused=true;}}}}
        paused
    }
    pub fn resume(&self,ids:Option<&[String]>)->Vec<(String,usize)> {
        let mut records=self.records.lock().expect("monitor records");
        let ids=ids.map(<[String]>::to_vec).unwrap_or_else(||records.keys().chain(self.files.keys()).cloned().collect());
        let mut resumed=vec![];
        for id in ids {
            if let Some(dropped)=records.get_mut(&id).and_then(CommandMonitor::resume) {resumed.push((id,dropped));continue;}
            if let Some((file,_))=self.files.get(&id) {
                let events={let mut file=file.lock().expect("file monitor");if !file.paused||file.settled {continue;}file.paused=false;file.check().unwrap_or_else(|error|file.stop(&format!("watcher error: {error}")).into_iter().collect())};
                if let Some(snapshot)=self.file_snapshots.lock().expect("file snapshots").get_mut(&id) {snapshot.paused=false;}
                resumed.push((id,0));for event in events {(self.emit)(event);}
            }
        }
        resumed
    }
    pub fn register(&mut self,runtime:&crate::runtime_session::TerminalRuntimeSession,mut record:CommandMonitor)->Result<(),crate::runtime_session::RuntimeError> {
        let (history,mut output)=runtime.subscribe_output()?;
        let mut exit=runtime.subscribe_exit();
        record.snapshot.started_at_ms=now_ms();
        let id=record.snapshot.id.clone();
        let initial=record.consume(&history,now_ms());
        self.records.lock().expect("monitor records").insert(id.clone(),record);
        for event in initial {(self.emit)(event);}
        let records=self.records.clone();let emit=self.emit.clone();
        self.tasks.retain(|task|!task.is_finished());
        self.tasks.push(tokio::spawn(async move {
            loop {
                let settled=exit.borrow().clone();
                if let Some(result)=settled {
                    let Some(mut record)=records.lock().expect("monitor records").remove(&id) else {return;};
                    while let Ok(chunk)=output.try_recv() {for event in record.consume(&chunk,now_ms()) {emit(event);}}
                    let summary=match result {Ok(result)=>format!("watcher {}{}",crate::tools::spawn::describe_exit(Some(&result)).unwrap_or_else(||"exited".to_owned()),result.exit_code.map_or(String::new(),|code|format!(" (exit code {code})"))),Err(error)=>format!("watcher error: {error}")};
                    if let Some(event)=record.settle(summary,now_ms()) {emit(event);}
                    return;
                }
                tokio::select! {
                    chunk=output.recv()=>{let Some(chunk)=chunk else {return;};let events=records.lock().expect("monitor records").get_mut(&id).map(|record|record.consume(&chunk,now_ms())).unwrap_or_default();for event in events {emit(event);}},
                    changed=exit.changed()=>{if changed.is_err() {return;}}
                }
            }
        }));
        Ok(())
    }
    pub fn dispose(&mut self) {for (_,(_,task)) in std::mem::take(&mut self.files) {task.abort();}self.file_snapshots.lock().expect("file snapshots").clear();for task in self.tasks.drain(..) {task.abort();}self.records.lock().expect("monitor records").clear();}
}
impl Drop for MonitorRegistry {fn drop(&mut self) {self.dispose();}}

#[cfg(test)]
mod registry_tests {
    use super::*;
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
