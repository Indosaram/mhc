use maho_pty::PtyExit;
pub fn describe_exit(exit:Option<&PtyExit>)->Option<String> {let exit=exit?;Some(if exit.timed_out {"timed_out".to_owned()} else if exit.cancelled {"killed".to_owned()} else if exit.exit_code==Some(0) {"completed".to_owned()} else if let Some(code)=exit.exit_code {format!("exited_{code}")} else {"exited".to_owned()})}
#[cfg(test)]
mod tests {use super::*;#[test] fn exit_priority() {assert_eq!(describe_exit(None),None);assert_eq!(describe_exit(Some(&PtyExit {exit_code:Some(0),cancelled:true,timed_out:true})),Some("timed_out".to_owned()));assert_eq!(describe_exit(Some(&PtyExit {exit_code:Some(9),cancelled:false,timed_out:false})),Some("exited_9".to_owned()));}}
