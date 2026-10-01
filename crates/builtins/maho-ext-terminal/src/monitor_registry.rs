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

#[cfg(test)]
mod tests {
    use super::*;
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
