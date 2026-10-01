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
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn live_tick_deduplicates_and_stop_drops_snapshot() {let mut ticker=MonitorStatusTicker::default();let mut rendered=Vec::new();ticker.sync(vec![MonitorSnapshotEntry {id:"bash_1".to_owned(),description:"build".to_owned(),started_at_ms:0.0,..Default::default()}],0.0,|status|rendered.push(status));assert!(ticker.running());ticker.tick(0.0,|status|rendered.push(status));assert_eq!(rendered.len(),1);ticker.tick(1000.0,|status|rendered.push(status));assert_eq!(rendered.len(),2);ticker.stop();assert!(!ticker.running());ticker.sync(Vec::new(),2000.0,|status|rendered.push(status));assert_eq!(rendered.last(),Some(&None));}
}
