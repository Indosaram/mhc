use std::{path::Path, process::Stdio, time::Duration};
use maho_ext_api::{ExecOptions, ExecResult};
use tokio::io::AsyncReadExt;

pub async fn exec_command(command: &str, args: &[String], cwd: &Path, options: ExecOptions) -> ExecResult {
    let mut command = tokio::process::Command::new(command);
    command.args(args).current_dir(options.cwd.as_deref().unwrap_or(cwd))
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return ExecResult { stdout: String::new(), stderr: error.to_string(), code: 1, killed: false },
    };
    let pre_aborted = options.signal.as_ref().is_some_and(|signal| signal.is_aborted());
    if pre_aborted { terminate(&mut child); }
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_buffer = [0; 8192];
    let mut err_buffer = [0; 8192];
    let mut status = None;
    let mut killed = pre_aborted;
    let mut idle_deadline = None;
    let mut kill_deadline = pre_aborted.then(|| tokio::time::Instant::now() + Duration::from_secs(5));
    let timeout = options.timeout_ms.filter(|ms| *ms > 0).map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
    loop {
        if status.is_some() && stdout.is_none() && stderr.is_none() { break; }
        tokio::select! {
            result = child.wait(), if status.is_none() => {
                status = Some(result.map(|status| status.code().unwrap_or(0)).unwrap_or(1));
                idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250));
            }
            result = async { stdout.as_mut().expect("open stdout").read(&mut out_buffer).await }, if stdout.is_some() => {
                match result {
                    Ok(0) | Err(_) => stdout = None,
                    Ok(count) => { out.extend_from_slice(&out_buffer[..count]); if status.is_some() { idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250)); } }
                }
            }
            result = async { stderr.as_mut().expect("open stderr").read(&mut err_buffer).await }, if stderr.is_some() => {
                match result {
                    Ok(0) | Err(_) => stderr = None,
                    Ok(count) => { err.extend_from_slice(&err_buffer[..count]); if status.is_some() { idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250)); } }
                }
            }
            () = async { options.signal.as_ref().expect("abort signal").cancelled().await }, if options.signal.is_some() && !killed => {
                killed = true;
                terminate(&mut child);
                kill_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
            }
            () = deadline(timeout), if timeout.is_some() && !killed => {
                killed = true;
                terminate(&mut child);
                kill_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
            }
            () = deadline(kill_deadline), if kill_deadline.is_some() && status.is_none() => {
                let _ = child.start_kill();
                status = Some(child.wait().await.map(|status| status.code().unwrap_or(0)).unwrap_or(1));
                idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250));
            }
            () = deadline(idle_deadline), if idle_deadline.is_some() => break,
        }
    }
    ExecResult { stdout: String::from_utf8_lossy(&out).into_owned(), stderr: String::from_utf8_lossy(&err).into_owned(), code: status.unwrap_or(0), killed }
}

async fn deadline(deadline: Option<tokio::time::Instant>) {
    tokio::time::sleep_until(deadline.expect("enabled deadline")).await;
}

#[cfg(unix)]
fn terminate(child: &mut tokio::process::Child) {
    if let Some(id) = child.id() {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(id as i32), nix::sys::signal::Signal::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate(child: &mut tokio::process::Child) { let _ = child.start_kill(); }
