use crate::monitor_registry::MonitorEvent;
pub const MONITOR_NOTIFICATION_CUSTOM_TYPE:&str="senpi-monitor:notification";
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
