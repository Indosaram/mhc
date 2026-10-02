use std::{collections::BTreeMap, path::Path};

#[derive(Debug, PartialEq)]
pub struct SupervisorChildExit { pub code: Option<f64>, pub signal: Option<String> }
#[derive(Debug, PartialEq, Eq)]
pub enum SupervisorRuntimePlatform { Posix, Win32 }

pub fn parse_supervisor_child_exit(text: &str) -> Option<SupervisorChildExit> {
    let value: serde_json::Value = serde_json::from_str(text.trim().lines().last()?).ok()?;
    let code = value.get("code")?;
    let signal = value.get("signal")?;
    if (!code.is_number() && !code.is_null()) || (!signal.is_string() && !signal.is_null()) || signal == "MODEL_PID" { return None; }
    Some(SupervisorChildExit { code:code.as_f64(), signal:signal.as_str().map(str::to_owned) })
}

pub fn get_supervisor_runtime_platform(env: &BTreeMap<String, String>, platform: &str) -> SupervisorRuntimePlatform {
    if env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").is_some_and(|value| value == "1") {
        match env.get("OMO_MEMORY_SUPERVISOR_PLATFORM").map(String::as_str) { Some("posix") => return SupervisorRuntimePlatform::Posix, Some("win32") => return SupervisorRuntimePlatform::Win32, _ => {} }
    }
    if platform == "win32" { SupervisorRuntimePlatform::Win32 } else { SupervisorRuntimePlatform::Posix }
}

pub fn read_injected_clock(directory: &Path) -> f64 {
    let Ok(entries) = std::fs::read_dir(directory) else { return f64::NAN; };
    entries.flatten().filter_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (sequence, time) = name.split_once('-')?;
        if sequence.is_empty() || !sequence.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
        let digits = time.strip_prefix('-').unwrap_or(time);
        let mut parts = digits.split('.');
        let integer = parts.next()?;
        if integer.is_empty() || !integer.bytes().all(|byte| byte.is_ascii_digit()) { return None; }
        if let Some(fraction) = parts.next() && (fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit())) { return None; }
        if parts.next().is_some() { return None; }
        Some((sequence.parse::<f64>().ok()?, time.parse::<f64>().ok()?))
    }).max_by(|left, right| left.0.total_cmp(&right.0)).map_or(f64::NAN, |(_, time)| time)
}

pub fn read_supervisor_clock_now(env: &BTreeMap<String, String>, now: impl FnOnce() -> f64) -> f64 {
    if env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").is_some_and(|value| value == "1") && let Some(path) = env.get("OMO_MEMORY_SUPERVISOR_CLOCK_PATH") { return read_injected_clock(Path::new(path)); }
    now()
}

pub fn get_supervisor_process_start(pid: u32) -> Option<String> { memory_core::locks::get_process_start_identity(pid) }

pub struct CancelSupervisorDeadline(Option<tokio::task::JoinHandle<()>>);
impl CancelSupervisorDeadline {
    pub fn cancel(&mut self) { if let Some(task) = self.0.take() { task.abort(); } }
}
impl Drop for CancelSupervisorDeadline {
    fn drop(&mut self) { self.cancel(); }
}

fn record_test_termination(env: &BTreeMap<String, String>, action: &str, pid: u32) -> std::io::Result<()> {
    if env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").is_some_and(|value| value == "1") && let Some(directory) = env.get("OMO_MEMORY_SUPERVISOR_TASKKILL_RUN_DIR") {
        std::fs::write(Path::new(directory).join(format!("{action}-{}.json",std::process::id())),format!("{}\n",serde_json::json!({"targetPid":pid})))?;
    }
    Ok(())
}

fn test_command(env: &BTreeMap<String,String>, name: &str) -> std::io::Result<Option<Vec<String>>> {
    if env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").is_none_or(|value| value != "1") { return Ok(None); }
    let Some(raw)=env.get(name) else { return Ok(None); };
    let command: Vec<String>=serde_json::from_str(raw).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput,format!("{name} must be a non-empty string array")))?;
    if command.is_empty() { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,format!("{name} must be a non-empty string array"))); }
    Ok(Some(command))
}

fn spawn_termination_command(command: &[String], args: &[String], env: &BTreeMap<String,String>, synchronous: bool) -> std::io::Result<()> {
    let mut child=std::process::Command::new(&command[0]);
    child.args(&command[1..]).args(args).envs(env).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if synchronous { child.status()?; } else { let mut child=child.spawn()?; std::thread::spawn(move || { if let Err(error)=child.wait(){eprintln!("termination command wait failed: {error}");} }); }
    Ok(())
}

pub fn signal_supervisor_process_group(pid: u32, signal: &str, env: &BTreeMap<String,String>) -> std::io::Result<()> {
    record_test_termination(env,&format!("posix-{signal}"),pid)?;
    if let Some(command)=test_command(env,"OMO_MEMORY_SUPERVISOR_POSIX_SIGNAL_COMMAND")? { return spawn_termination_command(&command,&["--signal-group".into(),pid.to_string(),signal.into()],env,false); }
    #[cfg(unix)] {
        use nix::sys::signal::{kill,Signal};
        let signal=signal.parse::<Signal>().map_err(std::io::Error::other)?;
        match kill(nix::unistd::Pid::from_raw(-(pid as i32)),signal) { Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()), Err(error) => Err(std::io::Error::from_raw_os_error(error as i32)) }
    }
    #[cfg(not(unix))] { Err(std::io::Error::new(std::io::ErrorKind::Unsupported,"POSIX process groups are unavailable")) }
}

pub fn record_supervisor_graceful_deadline(pid: Option<u32>, env: &BTreeMap<String,String>) -> std::io::Result<()> {
    match pid { Some(pid)=>record_test_termination(env,"win32-graceful",pid),None=>Ok(()) }
}

pub fn terminate_supervisor_child_gracefully(platform: SupervisorRuntimePlatform, wrapper: &mut tokio::process::Child, env: &BTreeMap<String,String>) -> std::io::Result<()> {
    match platform {
        SupervisorRuntimePlatform::Win32 => {
            record_supervisor_graceful_deadline(wrapper.id(),env)?;
            #[cfg(unix)]
            {
                let Some(pid)=wrapper.id()else{return Ok(());};
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32),nix::sys::signal::Signal::SIGTERM){
                    Ok(())|Err(nix::errno::Errno::ESRCH)=>Ok(()),
                    Err(error)=>Err(std::io::Error::from_raw_os_error(error as i32)),
                }
            }
            #[cfg(not(unix))]
            { wrapper.start_kill() }
        },
        SupervisorRuntimePlatform::Posix => match wrapper.id() { Some(pid)=>signal_supervisor_process_group(pid,"SIGTERM",env),None=>Ok(()) },
    }
}

pub fn terminate_supervisor_child_hard(platform: SupervisorRuntimePlatform, pid: Option<u32>, synchronous: bool, env: &BTreeMap<String,String>) -> std::io::Result<()> {
    let Some(pid)=pid else { return Ok(()); };
    match platform {
        SupervisorRuntimePlatform::Win32 => { let command=test_command(env,"OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND")?.unwrap_or_else(||vec!["taskkill".into()]); spawn_termination_command(&command,&["/pid".into(),pid.to_string(),"/T".into(),"/F".into()],env,synchronous) },
        SupervisorRuntimePlatform::Posix => signal_supervisor_process_group(pid,"SIGKILL",env),
    }
}

pub fn schedule_supervisor_deadline(instant: f64, env: &BTreeMap<String, String>, now: impl FnOnce() -> f64, callback: impl FnOnce() + Send + 'static) -> std::io::Result<CancelSupervisorDeadline> {
    use notify::Watcher;
    let directory = env.get("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS").filter(|value| *value == "1").and_then(|_| env.get("OMO_MEMORY_SUPERVISOR_CLOCK_PATH"));
    let Some(directory) = directory else {
        let delay = std::time::Duration::from_secs_f64((instant-now()).max(0.0)/1000.0);
        return Ok(CancelSupervisorDeadline(Some(tokio::spawn(async move { tokio::time::sleep(delay).await; callback(); }))));
    };
    let directory = std::path::PathBuf::from(directory);
    if read_injected_clock(&directory) >= instant { callback(); return Ok(CancelSupervisorDeadline(None)); }
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| { let _ = events.send(event); }).map_err(std::io::Error::other)?;
    watcher.watch(&directory, notify::RecursiveMode::NonRecursive).map_err(std::io::Error::other)?;
    Ok(CancelSupervisorDeadline(Some(tokio::spawn(async move {
        let watcher = watcher;
        let mut safety = tokio::time::interval_at(tokio::time::Instant::now()+std::time::Duration::from_millis(25),std::time::Duration::from_millis(25));
        loop {
            let time=read_injected_clock(&directory);
            if time.is_finite() && time >= instant { drop(watcher); callback(); return; }
            tokio::select! { _ = safety.tick() => {}, _ = received.recv() => {} }
        }
    }))))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exit_parser_uses_last_complete_line_and_rejects_pid_marker() { assert_eq!(parse_supervisor_child_exit("noise\n{\"code\":0,\"signal\":null}\n"),Some(SupervisorChildExit { code:Some(0.0), signal:None })); for invalid in ["","{\"code\":42,\"signal\":\"MODEL_PID\"}","{\"code\":\"0\",\"signal\":null}","{\"code\":0}"] { assert!(parse_supervisor_child_exit(invalid).is_none()); } }
    #[test] fn platform_override_is_test_seam_gated() { let mut env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_PLATFORM".into(),"win32".into())]); assert_eq!(get_supervisor_runtime_platform(&env,"linux"),SupervisorRuntimePlatform::Posix); env.insert("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS".into(),"1".into()); assert_eq!(get_supervisor_runtime_platform(&env,"linux"),SupervisorRuntimePlatform::Win32); }
    #[test] fn clock_uses_highest_numeric_sequence_not_time_or_filename_order() { let root=tempfile::tempdir().unwrap(); for name in ["2-200","10--4.5","junk","11-invalid","9-1000"] { std::fs::write(root.path().join(name),"").unwrap(); } assert_eq!(read_injected_clock(root.path()),-4.5); let env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_CLOCK_PATH".into(),root.path().to_string_lossy().into_owned())]); assert_eq!(read_supervisor_clock_now(&env,||12.0),12.0); }
    #[test] fn real_process_identity_matches_core() { assert_eq!(get_supervisor_process_start(std::process::id()),memory_core::locks::get_process_start_identity(std::process::id())); }
    #[tokio::test(start_paused=true)] async fn wall_deadline_fires_once_and_cancel_suppresses_callback() {
        let (sent,received)=tokio::sync::oneshot::channel();
        let _deadline=schedule_supervisor_deadline(100.0,&BTreeMap::new(),||0.0,move || { let _=sent.send(()); }).unwrap();
        received.await.unwrap();
        let (sent,received)=tokio::sync::oneshot::channel();
        let mut deadline=schedule_supervisor_deadline(100.0,&BTreeMap::new(),||0.0,move || { let _=sent.send(()); }).unwrap();
        deadline.cancel(); assert!(received.await.is_err());
    }
    #[tokio::test] async fn injected_deadline_observes_sequence_clock_publication() {
        let root=tempfile::tempdir().unwrap(); std::fs::write(root.path().join("1-10"),"").unwrap();
        let env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS".into(),"1".into()),("OMO_MEMORY_SUPERVISOR_CLOCK_PATH".into(),root.path().to_string_lossy().into_owned())]);
        let (sent,received)=tokio::sync::oneshot::channel();
        let _deadline=schedule_supervisor_deadline(20.0,&env,||0.0,move || { let _=sent.send(()); }).unwrap();
        std::fs::write(root.path().join("2-20"),"").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5),received).await.unwrap().unwrap();
    }
    #[test] fn termination_seams_require_explicit_enable_and_validate_arrays() {
        let mut env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND".into(),"[]".into())]);
        assert!(test_command(&env,"OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND").unwrap().is_none());
        env.insert("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS".into(),"1".into());
        assert!(test_command(&env,"OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND").is_err());
        for raw in ["null","[1]","\"command\""] { env.insert("OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND".into(),raw.into()); assert!(test_command(&env,"OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND").is_err()); }
    }
    #[test] fn graceful_deadline_records_target_and_missing_pid_is_noop() {
        let root=tempfile::tempdir().unwrap(); let env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS".into(),"1".into()),("OMO_MEMORY_SUPERVISOR_TASKKILL_RUN_DIR".into(),root.path().to_string_lossy().into_owned())]);
        record_supervisor_graceful_deadline(Some(42),&env).unwrap();
        let value: serde_json::Value=serde_json::from_slice(&std::fs::read(root.path().join(format!("win32-graceful-{}.json",std::process::id()))).unwrap()).unwrap(); assert_eq!(value["targetPid"],42);
        terminate_supervisor_child_hard(SupervisorRuntimePlatform::Win32,None,false,&env).unwrap();
    }
    #[cfg(unix)]
    #[tokio::test] async fn posix_graceful_termination_reaps_real_process_group() {
        let mut command=tokio::process::Command::new("/bin/sh"); command.args(["-c","read line"]).stdin(std::process::Stdio::piped()).process_group(0);
        let mut child=command.spawn().unwrap();
        terminate_supervisor_child_gracefully(SupervisorRuntimePlatform::Posix,&mut child,&BTreeMap::new()).unwrap();
        use std::os::unix::process::ExitStatusExt;
        let status=tokio::time::timeout(std::time::Duration::from_secs(5),child.wait()).await.unwrap().unwrap(); assert_eq!(status.signal(),Some(15));
    }
    #[cfg(unix)]
    #[test] fn windows_taskkill_seam_preserves_tree_arguments() {
        let root=tempfile::tempdir().unwrap(); let output=root.path().join("arguments");
        let env=BTreeMap::from([("OMO_MEMORY_SUPERVISOR_ALLOW_TEST_SEAMS".into(),"1".into()),("OUTPUT".into(),output.to_string_lossy().into_owned()),("OMO_MEMORY_SUPERVISOR_TASKKILL_COMMAND".into(),serde_json::json!(["/bin/sh","-c","printf '%s\\n' \"$@\" > \"$OUTPUT\"","seam"]).to_string())]);
        terminate_supervisor_child_hard(SupervisorRuntimePlatform::Win32,Some(42),true,&env).unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(),"/pid\n42\n/T\n/F\n");
    }
}
