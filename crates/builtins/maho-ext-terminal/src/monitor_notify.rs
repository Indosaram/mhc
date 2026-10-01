use crate::monitor_registry::MonitorEvent;
pub const MONITOR_NOTIFICATION_CUSTOM_TYPE:&str="senpi-monitor:notification";
fn identity(event:&MonitorEvent)->(&str,&str,&str) {match event {MonitorEvent::Line {id,description,..}=>(id,description,"line"),MonitorEvent::Summary {id,description,..}=>(id,description,"summary")}}
#[derive(Default)]
pub struct MonitorDeliveryQueue {
    events:Vec<MonitorEvent>,
    event_chars:usize,
    overflow:Vec<(String,String,usize,Vec<String>)>,
    last_injection:std::collections::BTreeMap<String,f64>,
    last_batch:std::collections::BTreeMap<String,String>,
    consecutive_wakes:usize,
    last_wake:Option<f64>,
}
pub struct MonitorInjection {pub content:String,pub details:serde_json::Value,pub pause_ids:Vec<String>}
impl MonitorDeliveryQueue {
    pub fn notify(&mut self,event:MonitorEvent,settings:&crate::settings::MonitorDeliverySettings) {
        let (id,description,kind)=identity(&event);
        if kind=="summary" {self.last_injection.remove(id);self.last_batch.remove(id);}
        let chars=format!("Monitor event({description}): {}",event_body(&event)).encode_utf16().count();
        if kind!="summary"&&(self.events.len()>=settings.max_lines_per_injection as usize||self.event_chars+chars>(settings.max_chars_per_injection as usize).saturating_sub(512).max(1)) {
            if let Some((_,_,count,kinds))=self.overflow.iter_mut().find(|(own,_,_,_)|own==id) {*count+=1;if !kinds.iter().any(|own|own==kind) {kinds.push(kind.to_owned());}}
            else {self.overflow.push((id.to_owned(),description.to_owned(),1,vec![kind.to_owned()]));}
        } else {self.events.push(event);self.event_chars+=chars;}
    }
    pub fn note_activity(&mut self) {self.consecutive_wakes=0;}
    pub fn resume(&mut self,ids:&[String]) {for id in ids {self.last_injection.remove(id);self.last_batch.remove(id);}self.note_activity();}
    fn pending_ids(&self)->Vec<String> {let mut ids=vec![];for id in self.events.iter().map(|event|identity(event).0).chain(self.overflow.iter().map(|(id,_,_,_)|id.as_str())) {if !ids.iter().any(|own|own==id) {ids.push(id.to_owned());}}ids}
    pub fn next_rate_limit(&self,now:f64,settings:&crate::settings::MonitorDeliverySettings)->Option<f64> {self.pending_ids().iter().map(|id|(self.last_injection.get(id).copied().unwrap_or(now)+settings.rate_limit_ms-now).max(1.0)).min_by(f64::total_cmp)}
    pub fn flush(&mut self,now:f64,settings:&crate::settings::MonitorDeliverySettings)->Option<MonitorInjection> {
        let ready=self.pending_ids().into_iter().filter(|id|self.events.iter().any(|event|identity(event).0==id&&identity(event).2=="summary")||self.last_injection.get(id).is_none_or(|last|now-last>=settings.rate_limit_ms)).collect::<Vec<_>>();
        if ready.is_empty() {return None;}
        let mut fingerprints=std::collections::BTreeMap::new();
        for id in &ready {
            let batch=self.events.iter().filter(|event|identity(event).0==id).collect::<Vec<_>>();
            if !batch.is_empty()&&batch.iter().all(|event|identity(event).2=="line")&&!self.overflow.iter().any(|(own,_,count,_)|own==id&&*count>0) {fingerprints.insert(id.clone(),batch.into_iter().map(event_body).collect::<Vec<_>>().join("\n"));}
        }
        let injected=ready.iter().filter(|id|fingerprints.get(*id).is_none_or(|fingerprint|self.last_batch.get(*id)!=Some(fingerprint))).cloned().collect::<Vec<_>>();
        let selected=self.events.iter().filter(|event|injected.iter().any(|id|id==identity(event).0)).cloned().collect::<Vec<_>>();
        self.events.retain(|event|!ready.iter().any(|id|id==identity(event).0));
        self.event_chars=self.events.iter().map(|event|format!("Monitor event({}): {}",identity(event).1,event_body(event)).encode_utf16().count()).sum();
        if injected.is_empty() {return None;}
        let overflow_count=self.overflow.iter().filter(|(id,_,_,_)|injected.contains(id)).map(|(_,_,count,_)|count).sum();
        if self.last_wake.is_some_and(|last|now-last>settings.rate_limit_ms*2.0) {self.consecutive_wakes=0;}
        let completion=selected.iter().any(|event|matches!(event,MonitorEvent::Summary {summary,..} if summary!=crate::shared::FIRE_BUDGET_AUTO_MUTE_SUMMARY));
        let reaches_budget=!completion&&self.consecutive_wakes+1>=settings.wake_budget as usize;
        let notice=if reaches_budget {"Monitor paused after repeated updates. Completion still wakes this session; peek bash_output or re-arm only for intermediate events."} else {""};
        let content=build_monitor_message(&selected,overflow_count,notice,settings.max_chars_per_injection as usize);
        let details=serde_json::json!({"monitors":injected.iter().map(|id| {
            let own=selected.iter().filter(|event|identity(event).0==id).collect::<Vec<_>>();let overflow=self.overflow.iter().find(|(own,_,_,_)|own==id);
            let mut kinds=vec![];for kind in own.iter().map(|event|identity(event).2).chain(overflow.into_iter().flat_map(|(_,_,_,kinds)|kinds.iter().map(String::as_str))) {if !kinds.contains(&kind) {kinds.push(kind);}}
            serde_json::json!({"id":id,"description":own.first().map(|event|identity(event).1).or_else(||overflow.map(|(_,description,_,_)|description.as_str())).unwrap_or(id),"eventCount":own.len()+overflow.map_or(0,|(_,_,count,_)|*count),"kinds":kinds})
        }).collect::<Vec<_>>()});
        self.last_wake=Some(now);self.consecutive_wakes=if completion {0} else {self.consecutive_wakes+1};
        for id in &injected {self.last_injection.insert(id.clone(),now);if let Some(batch)=fingerprints.remove(id) {self.last_batch.insert(id.clone(),batch);} else {self.last_batch.remove(id);}}
        self.overflow.retain(|(id,_,_,_)|!injected.contains(id));
        if reaches_budget {self.events.clear();self.event_chars=0;self.overflow.clear();self.consecutive_wakes=0;}
        Some(MonitorInjection {content,details,pause_ids:if reaches_budget {injected} else {vec![]}})
    }
}
pub fn event_body(event:&MonitorEvent)->String {
    let text=match event {MonitorEvent::Line {line,..}=>line,MonitorEvent::Summary {summary,..}=>summary};
    let sanitized=crate::output_format::sanitize_terminal_output(text);
    let mut result=String::new();let mut newline=false;
    for ch in sanitized.chars() {if matches!(ch,'\r'|'\n') {if !newline {result.push(' ');}newline=true;} else {result.push(ch);newline=false;}}
    result.trim_end().to_owned()
}
pub fn build_monitor_message(events:&[MonitorEvent],overflow_count:usize,pause_notice:&str,max_chars:usize)->String {
    let mut groups:Vec<(&str,&str,Vec<String>)>=vec![];
    for event in events {
        let (id,description)=match event {MonitorEvent::Line {id,description,..}|MonitorEvent::Summary {id,description,..}=>(id.as_str(),description.as_str())};
        let body=event_body(event);
        if let Some((_,_,bodies))=groups.iter_mut().find(|(own,_,_)|*own==id) {bodies.push(body);} else {groups.push((id,description,vec![body]));}
    }
    let body=groups.into_iter().map(|(_,description,bodies)|format!("Monitor event({description}): {}",bodies.join("\n"))).collect::<Vec<_>>().join("\n");
    let overflow=if overflow_count>0 {format!("[{overflow_count} additional event lines omitted; peek bash_output for full history.]")} else {String::new()};
    let suffix=[overflow.as_str(),pause_notice].into_iter().filter(|text|!text.is_empty()).collect::<Vec<_>>().join("\n");
    let fixed="<system-reminder>".len()+"</system-reminder>".len()+if suffix.is_empty() {0} else {suffix.encode_utf16().count()+1};
    let maximum=max_chars.saturating_sub(fixed);let units=body.encode_utf16().collect::<Vec<_>>();
    let body=if units.len()<=maximum {body} else if maximum<=3 {String::from_utf16_lossy(&units[..maximum])} else {format!("{}...",String::from_utf16_lossy(&units[..maximum-3]))};
    format!("<system-reminder>{body}{}{suffix}</system-reminder>",if !body.is_empty()&&!suffix.is_empty() {"\n"} else {""})
}
#[cfg(test)]
mod tests {
    use super::*;
    fn line(text:&str)->MonitorEvent {MonitorEvent::Line {id:"b1".to_owned(),description:"watch".to_owned(),line:text.to_owned()}}
    #[test]
    fn rate_limit_and_duplicate_batches_do_not_consume_wake_budget() {
        let settings=crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;let mut queue=MonitorDeliveryQueue::default();
        queue.notify(line("one"),&settings);assert!(queue.flush(0.0,&settings).is_some());
        queue.notify(line("one"),&settings);assert!(queue.flush(100.0,&settings).is_none());assert!(queue.flush(5000.0,&settings).is_none());
        assert_eq!(queue.consecutive_wakes,1);assert!(queue.pending_ids().is_empty());
        queue.notify(line("two"),&settings);assert!(queue.flush(5001.0,&settings).is_some());assert_eq!(queue.consecutive_wakes,2);
    }
    #[test]
    fn completion_bypasses_rate_limit_and_wake_budget_mutes_only_updates() {
        let mut settings=crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;settings.wake_budget=1.0;
        let mut queue=MonitorDeliveryQueue::default();queue.notify(line("one"),&settings);
        assert_eq!(queue.flush(0.0,&settings).unwrap().pause_ids,["b1"]);
        queue.notify(MonitorEvent::Summary {id:"b1".to_owned(),description:"watch".to_owned(),summary:"watcher completed".to_owned()},&settings);
        assert!(queue.flush(1.0,&settings).unwrap().pause_ids.is_empty());
    }
    #[test]
    fn event_format_groups_by_runtime_and_sanitizes_newlines() {
        let events=vec![MonitorEvent::Line {id:"b1".to_owned(),description:"one".to_owned(),line:"first\r\nsecond\n".to_owned()},MonitorEvent::Line {id:"b2".to_owned(),description:"two".to_owned(),line:"other".to_owned()},MonitorEvent::Summary {id:"b1".to_owned(),description:"one".to_owned(),summary:"watcher completed".to_owned()}];
        assert_eq!(build_monitor_message(&events,0,"",4096),"<system-reminder>Monitor event(one): first second\nwatcher completed\nMonitor event(two): other</system-reminder>");
    }
    #[test]
    fn omitted_lines_and_pause_notice_survive_body_clipping() {
        let events=vec![MonitorEvent::Line {id:"b1".to_owned(),description:"one".to_owned(),line:"x".repeat(500)}];
        let text=build_monitor_message(&events,3,"paused",150);
        assert_eq!(text.encode_utf16().count(),150);assert!(text.contains("...\n[3 additional event lines omitted; peek bash_output for full history.]\npaused</system-reminder>"));
    }
}
