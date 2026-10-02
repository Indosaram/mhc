use crate::runtime_session::TerminalRuntimeSession;
use super::context::{TerminalToolResult,error_result,text_result};
pub fn execute_bash_resize(runtime:Option<&TerminalRuntimeSession>,id:&str,cols:f64,rows:f64)->TerminalToolResult {
    let Some(runtime)=runtime else {return error_result(format!("No terminal session found with id: {id}"));};
    match runtime.exited() {Ok(true)=>return error_result(format!("Session {id} is not running; cannot resize.")),Err(error)=>return error_result(error.to_string()),Ok(false)=>{}}
    if !cols.is_finite()||!rows.is_finite()||cols<1.0||rows<1.0 {return error_result("cols and rows must be finite integers >= 1.");}
    let cols=cols.trunc() as u16;let rows=rows.trunc() as u16;
    match runtime.resize(cols,rows) {Err(error)=>text_result(format!("Resize note for {id}: {error}")),Ok(())=>text_result(format!("Resized {id} to {cols}x{rows}."))}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn resized_geometry_is_visible_inside_live_pty()->Result<(),crate::runtime_session::RuntimeError> {
        let mut runtime=TerminalRuntimeSession::start("geometry",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; printf 'ready\\n'; read value; stty size"))?;
        let (history,mut output)=runtime.subscribe_output()?;let mut observed=history;
        tokio::time::timeout(std::time::Duration::from_secs(5),async {while !observed.contains("ready") {observed.push_str(&output.recv().await.unwrap());}}).await.unwrap();
        assert_eq!(execute_bash_resize(Some(&runtime),"bash_1",f64::NAN,24.0).is_error,Some(true));assert_eq!(execute_bash_resize(Some(&runtime),"bash_1",100.0,40.0).is_error,None);
        let mut exit=runtime.subscribe_exit();runtime.write(b"go\n")?;tokio::time::timeout(std::time::Duration::from_secs(5),async {while exit.borrow_and_update().is_none() {exit.changed().await.unwrap();}}).await.unwrap();assert!(runtime.full_output()?.contains("40 100"));assert_eq!(execute_bash_resize(Some(&runtime),"bash_1",80.0,24.0).is_error,Some(true));runtime.dispose()
    }
}
