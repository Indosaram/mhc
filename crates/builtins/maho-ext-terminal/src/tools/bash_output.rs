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
