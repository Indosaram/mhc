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
}
fn now_ms()->f64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0}
impl MonitorRegistry {
    pub fn new(emit:impl Fn(MonitorEvent)+Send+Sync+'static)->Self {Self {records:Default::default(),tasks:vec![],emit:std::sync::Arc::new(emit)}}
    pub fn snapshot(&self)->Vec<MonitorSnapshotEntry> {self.records.lock().expect("monitor records").values().map(|record|record.snapshot.clone()).collect()}
    pub fn pause(&self,ids:&[String])->Vec<String> {
        let mut records=self.records.lock().expect("monitor records");
        ids.iter().filter(|id|records.get_mut(*id).is_some_and(CommandMonitor::pause)).cloned().collect()
    }
    pub fn resume(&self,ids:Option<&[String]>)->Vec<(String,usize)> {
        let mut records=self.records.lock().expect("monitor records");
        let ids=ids.map(<[String]>::to_vec).unwrap_or_else(||records.keys().cloned().collect());
        ids.into_iter().filter_map(|id|records.get_mut(&id)?.resume().map(|dropped|(id,dropped))).collect()
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
    pub fn dispose(&mut self) {for task in self.tasks.drain(..) {task.abort();}self.records.lock().expect("monitor records").clear();}
}
impl Drop for MonitorRegistry {fn drop(&mut self) {self.dispose();}}

#[cfg(test)]
mod registry_tests {
    use super::*;
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
