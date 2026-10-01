use crate::runtime_session::TerminalRuntimeSession;
use super::context::{TerminalToolResult,error_result,text_result};
pub fn execute_bash_resize(runtime:Option<&TerminalRuntimeSession>,id:&str,cols:f64,rows:f64)->TerminalToolResult {
    let Some(runtime)=runtime else {return error_result(format!("No terminal session found with id: {id}"));};
    match runtime.exited() {Ok(true)=>return error_result(format!("Session {id} is not running; cannot resize.")),Err(error)=>return error_result(error.to_string()),Ok(false)=>{}}
    if !cols.is_finite()||!rows.is_finite()||cols<1.0||rows<1.0 {return error_result("cols and rows must be finite integers >= 1.");}
    let cols=cols.trunc() as u16;let rows=rows.trunc() as u16;
    match runtime.resize(cols,rows) {Err(error)=>text_result(format!("Resize note for {id}: {error}")),Ok(())=>text_result(format!("Resized {id} to {cols}x{rows}."))}
}
