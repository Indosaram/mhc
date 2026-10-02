use crate::runtime_session::{TerminalRuntimeSession,RuntimeError};
use crate::shared::safe_reg_exp;
use crate::output_format::format_terminal_tool_output;
use super::context::{TerminalToolResult,error_result,noticed_result};
use super::spawn::describe_exit;
pub fn execute_bash_output(runtime:Option<&TerminalRuntimeSession>,id:&str,filter:Option<&str>)->Result<TerminalToolResult,RuntimeError> {
    let Some(runtime)=runtime else {return Ok(error_result(format!("No terminal session found with id: {id}")));};
    let exit=runtime.exit_result()?;let status=match exit.as_ref() {None=>"status: running".to_owned(),Some(exit)=>{let label=describe_exit(Some(exit)).unwrap_or_else(||"exited".to_owned());match exit.exit_code {Some(code)=>format!("status: {label} exit_code: {code}"),None=>format!("status: {label}")}}};
    let delta=runtime.read_delta()?;let filtered=if let Some(regex)=filter.and_then(safe_reg_exp) {delta.text.split('\n').filter(|line|regex.is_match(line).unwrap_or(false)).collect::<Vec<_>>().join("\n")} else {delta.text};
    let formatted=format_terminal_tool_output(&filtered);let dropped_notice=if delta.dropped_chars>0 {Some(format!("[{} earlier chars dropped]",delta.dropped_chars))} else {None};let dropped=dropped_notice.as_ref().map(|notice|format!("{notice}\n")).unwrap_or_default();
    let text=format!("{status}\n{dropped}{}",if formatted.text.is_empty() {"(no new output)"} else {&formatted.text});
    Ok(noticed_result(&text,&[dropped_notice.as_deref(),formatted.marker.as_deref()]))
}
pub fn with_monitor_state(mut result:TerminalToolResult,id:&str,paused:bool,dropped:usize)->TerminalToolResult {
    let dropped=if paused {dropped} else {0};
    if paused&&let Some(part)=result.content.first_mut() {let note=if dropped>0 {format!("monitor muted — {dropped} line(s) dropped while muted; run monitor({{ action: \"rearm\", bash_id: \"{id}\" }}) to resume.")} else {format!("monitor muted; run monitor({{ action: \"rearm\", bash_id: \"{id}\" }}) to resume.")};part.text=format!("{note}\n{}",part.text);}
    let details=result.details.get_or_insert_with(Default::default);details.insert("monitorMuted".to_owned(),serde_json::json!(paused));details.insert("mutedDropped".to_owned(),serde_json::json!(dropped));result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn muted_log_metadata_reports_burned_lines_only_while_paused() {
        let paused=with_monitor_state(super::super::context::text_result("output"),"mon_saved",true,3);assert_eq!(paused.details.as_ref().unwrap()["monitorMuted"],true);assert_eq!(paused.details.as_ref().unwrap()["mutedDropped"],3);
        let live=with_monitor_state(super::super::context::text_result("output"),"mon_saved",false,3);assert_eq!(live.details.as_ref().unwrap()["monitorMuted"],false);assert_eq!(live.details.as_ref().unwrap()["mutedDropped"],0);assert_eq!(live.content[0].text,"output");
    }
}
