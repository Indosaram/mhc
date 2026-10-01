use crate::runtime_session::TerminalRuntimeSession;
use crate::shared::encode_keys;
use super::context::{TerminalToolResult,error_result,text_result};
pub struct BashInputInput<'a> {pub bash_id:&'a str,pub input:Option<&'a str>,pub keys:&'a [String],pub submit:Option<bool>}
pub fn execute_bash_input(runtime:Option<&mut TerminalRuntimeSession>,input:BashInputInput<'_>)->TerminalToolResult {
    let Some(runtime)=runtime else {return error_result(format!("No terminal session found with id: {}",input.bash_id));};
    match runtime.exited() {Ok(true)=>return error_result(format!("Session {} is not running; cannot send input.",input.bash_id)),Err(error)=>return error_result(error.to_string()),Ok(false)=>{}}
    let mut notes=Vec::new();
    if let Some(text)=input.input {let submit=input.submit.unwrap_or(!text.is_empty());let data=if submit {format!("{text}\r")} else {text.to_owned()};if let Err(error)=runtime.write(data.as_bytes()) {return error_result(format!("Failed to write input: {error}"));}notes.push(if submit {"wrote input + Enter".to_owned()} else {"wrote input".to_owned()});}
    if !input.keys.is_empty() {let keys=encode_keys(input.keys);if !keys.data.is_empty() {if let Err(error)=runtime.write(keys.data.as_bytes()) {return error_result(format!("Failed to send keys: {error}"));}notes.push(format!("sent {} key(s)",input.keys.len()-keys.unknown.len()));}
        if !keys.unknown.is_empty() {notes.push(format!("ignored unknown keys: {}",keys.unknown.join(", ")));}}
    if notes.is_empty() {return error_result("Nothing to send: provide `input` and/or `keys`.");}
    text_result(format!("Sent to {}: {}.",input.bash_id,notes.join("; ")))
}
#[cfg(test)]
mod tests {
    use super::*;use maho_pty::PtySessionOptions;use std::time::Duration;
    #[test] fn missing_input_and_real_pty_resize()->Result<(),crate::runtime_session::RuntimeError> {
        assert_eq!(execute_bash_input(None,BashInputInput {bash_id:"bash_999",input:Some("x"),keys:&[],submit:None}).is_error,Some(true));
        let runtime=TerminalRuntimeSession::start("read",PtySessionOptions::new("/bin/sh").arg("-c").arg("read value").timeout(Duration::from_secs(5)))?;
        let resized=super::super::bash_resize::execute_bash_resize(Some(&runtime),"bash_1",100.0,30.0);assert_eq!(resized.is_error,None);assert_eq!(resized.content[0].text,"Resized bash_1 to 100x30.");runtime.dispose()
    }
}
