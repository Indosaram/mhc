use maho_pty::PtyExit;
pub fn command_options(command:&str,cwd:Option<&std::path::Path>,shell_path:Option<&str>)->std::io::Result<maho_pty::PtySessionOptions> {
    let override_path=std::env::var("SENPI_GIT_BASH_PATH").ok();
    let selected=shell_path.filter(|path|!path.is_empty()).or(override_path.as_deref().filter(|path|!path.is_empty()));
    let shell=if let Some(path)=selected {
        if !std::path::Path::new(path).exists() {return Err(std::io::Error::other(if shell_path.is_some() {format!("Custom shell path not found: {path}")}else {format!("SENPI_GIT_BASH_PATH points to a missing shell: {path}")}));}path.to_owned()
    }else if std::path::Path::new("/bin/bash").exists() {"/bin/bash".to_owned()}else {"sh".to_owned()};
    let name=std::path::Path::new(&shell).file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let mut options=maho_pty::PtySessionOptions::new(shell);
    if matches!(name.as_str(),"cmd"|"cmd.exe") {options=options.arg("/c");}
    else if matches!(name.as_str(),"powershell"|"powershell.exe"|"pwsh"|"pwsh.exe") {options=options.arg("-NoProfile").arg("-Command");}
    else {options=options.arg("-c");}
    options=options.arg(command);if let Some(cwd)=cwd {options=options.cwd(cwd);}Ok(options)
}
pub fn describe_exit(exit:Option<&PtyExit>)->Option<String> {let exit=exit?;Some(if exit.timed_out {"timed_out".to_owned()} else if exit.cancelled {"killed".to_owned()} else if exit.exit_code==Some(0) {"completed".to_owned()} else if let Some(code)=exit.exit_code {format!("exited_{code}")} else {"exited".to_owned()})}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_shell_kind_controls_arguments_and_missing_path_fails()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;
        for (name,args) in [("cmd.exe",vec!["/c","echo ready"]),("pwsh",vec!["-NoProfile","-Command","echo ready"]),("sh",vec!["-c","echo ready"])] {
            let path=dir.path().join(name);std::fs::write(&path,b"")?;
            let options=command_options("echo ready",Some(dir.path()),Some(path.to_str().unwrap()))?;assert_eq!(options.args,args);assert_eq!(options.cwd.as_deref(),Some(dir.path()));
        }
        assert!(command_options("echo ready",None,Some(dir.path().join("missing").to_str().unwrap())).is_err());Ok(())
    }
    #[test] fn exit_priority() {assert_eq!(describe_exit(None),None);assert_eq!(describe_exit(Some(&PtyExit {exit_code:Some(0),cancelled:true,timed_out:true})),Some("timed_out".to_owned()));assert_eq!(describe_exit(Some(&PtyExit {exit_code:Some(9),cancelled:false,timed_out:false})),Some("exited_9".to_owned()));}
}
