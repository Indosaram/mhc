use maho_pty::PtyExit;
/// `spawnCommandSession` shell resolution: `getShellConfig(ctx.shellPath)` picks the shell and its
/// kind-specific args; stdin-transport shells (legacy WSL bash `-s`) get the command written after start.
pub fn command_options(command:&str,cwd:Option<&std::path::Path>,shell_path:Option<&str>)->std::io::Result<(maho_pty::PtySessionOptions,Option<String>)> {
    let shell=maho_tools::shell::get_shell_config(shell_path).map_err(std::io::Error::other)?;
    let use_stdin=shell.command_transport==Some("stdin");
    let is_windows_git_bash=cfg!(windows) && shell.kind==Some(maho_tools::shell::ShellKind::Bash) && shell.command_transport.is_none();
    let mut options=maho_pty::PtySessionOptions::new(shell.shell);options.args=if is_windows_git_bash {vec!["-lc".to_owned()]} else {shell.args};
    if !use_stdin {options=options.arg(command);}
    if let Some(cwd)=cwd {options=options.cwd(cwd);}
    // spawn.ts: `env: { ...ctx.getEnv(), ...request.envOverrides }`; getEnv is `getShellEnv()`. Later `.env()` pushes override.
    options.env=maho_core::utils::shell::get_shell_env().into_iter().collect();Ok((options,use_stdin.then(||format!("{command}\n"))))
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
            let (options,stdin)=command_options("echo ready",Some(dir.path()),Some(path.to_str().unwrap()))?;assert_eq!(options.args,args);assert_eq!(options.cwd.as_deref(),Some(dir.path()));assert_eq!(stdin,None);
        }
        assert!(command_options("echo ready",None,Some(dir.path().join("missing").to_str().unwrap())).is_err());Ok(())
    }
    #[test]
    fn command_env_puts_the_agent_bin_dir_on_path()->std::io::Result<()> {
        let (options,_)=command_options("echo ready",None,None)?;
        let bin=maho_core::config::get_bin_dir();let delimiter=if cfg!(windows) {';'} else {':'};
        let path=options.env.iter().find(|(key,_)|key.eq_ignore_ascii_case("path")).map(|(_,value)|value.clone()).unwrap_or_default();
        assert!(path.split(delimiter).any(|entry|entry==bin));Ok(())
    }
    #[test]
    fn legacy_wsl_bash_uses_stdin_transport() -> std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("bash.exe");std::fs::write(&path,b"")?;
        let input=format!("{}\\Windows\\System32\\bash.exe",dir.path().display());
        let (options,stdin)=command_options("echo ready",None,Some(&input))?;
        assert_eq!(options.args,["-s"]);assert_eq!(stdin,Some("echo ready\n".to_owned()));Ok(())
    }
    #[cfg(windows)]
    #[test]
    fn git_bash_uses_login_command_mode() -> std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("bash.exe");std::fs::write(&path,b"")?;
        let (options,stdin)=command_options("echo ready",None,Some(path.to_str().unwrap()))?;
        if cfg!(windows) {assert_eq!(options.args, ["-lc", "echo ready"]);} else {assert_eq!(options.args,["-c","echo ready"]);}
        assert_eq!(stdin,None);Ok(())
    }
    #[test] fn exit_priority() {assert_eq!(describe_exit(None),None);assert_eq!(describe_exit(Some(&PtyExit {exit_code:Some(0),cancelled:true,timed_out:true})),Some("timed_out".to_owned()));assert_eq!(describe_exit(Some(&PtyExit {exit_code:Some(9),cancelled:false,timed_out:false})),Some("exited_9".to_owned()));}
}
