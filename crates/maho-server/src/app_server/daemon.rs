use super::{daemon_occupancy::{AppServerListenOccupancy, inspect_listen_occupancy}, daemon_probe::{cleanup_state, poll_probe, probe_listen, read_settings}, daemon_process::{DaemonPidFile, parse_daemon_pid_file, process_matches_pid_file, stop_validated_pid, wait_for_start_time}, metadata_state::write_sidecar_file_atomic};
use serde_json::{Value, json};
use std::{path::{Path, PathBuf}, process::Stdio};

pub struct DaemonPaths {
    pub dir: PathBuf,
    pub pid_file: PathBuf,
    pub lock_file: PathBuf,
    pub settings_file: PathBuf,
    pub stderr_log: PathBuf,
    pub token_file: PathBuf,
}
impl DaemonPaths {
    pub fn new(agent_dir: &Path) -> Self {
        let dir = agent_dir.join("app-server-daemon");
        Self { pid_file:dir.join("app-server.pid"), lock_file:dir.join("daemon.lock"), settings_file:dir.join("settings.json"), stderr_log:dir.join("stderr.log"), token_file:agent_dir.join("app-server/ws-token"), dir }
    }
}
async fn read_pid(paths: &DaemonPaths) -> std::io::Result<Option<DaemonPidFile>> {
    match tokio::fs::read_to_string(&paths.pid_file).await {
        Ok(text) => Ok(parse_daemon_pid_file(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
pub async fn status_daemon(paths: &DaemonPaths, listen: &Value, version: &str) -> std::io::Result<Value> {
    let probe = probe_listen(&paths.token_file,listen,2000,version).await?;
    let pid = read_pid(paths).await?;
    let matches = match &pid {Some(pid) => process_matches_pid_file(pid).await?,None => false};
    if let Some(version) = probe {
        return Ok(match pid.filter(|_|matches) {Some(pid) => json!({"status":"running","pid":pid.pid,"listen":listen["url"],"version":version}),None => json!({"status":"running-unmanaged","listen":listen["url"],"version":version})});
    }
    if pid.is_some() && !matches {cleanup_state(&paths.pid_file,&paths.settings_file,listen).await?;}
    Ok(json!({"status":"not-running"}))
}
pub async fn stop_daemon(paths: &DaemonPaths, listen: &Value, version: &str) -> std::io::Result<Value> {
    let Some(pid) = read_pid(paths).await? else {
        if probe_listen(&paths.token_file,listen,2000,version).await?.is_none() {cleanup_state(&paths.pid_file,&paths.settings_file,listen).await?;}
        return Ok(json!({"status":"not-running"}));
    };
    if !process_matches_pid_file(&pid).await? {cleanup_state(&paths.pid_file,&paths.settings_file,listen).await?;return Ok(json!({"status":"not-running"}));}
    stop_validated_pid(&pid,"-TERM").await?;
    if process_matches_pid_file(&pid).await? {stop_validated_pid(&pid,"-KILL").await?;}
    cleanup_state(&paths.pid_file,&paths.settings_file,listen).await?;
    Ok(json!({"status":"stopped"}))
}
pub async fn start_daemon(paths: &DaemonPaths, listen: &Value, version: &str, executable: &Path, prefix_args: &[String]) -> std::io::Result<Value> {
    if let AppServerListenOccupancy::AppServer {version} = inspect_listen_occupancy(&paths.token_file,listen,version).await? {
        let mut output = json!({"status":"already-running","listen":listen["url"],"version":version});
        if let Some(pid) = read_pid(paths).await? && process_matches_pid_file(&pid).await? {output["pid"] = json!(pid.pid);}
        return Ok(output);
    }
    if let Some(pid) = read_pid(paths).await? && process_matches_pid_file(&pid).await? {
        if let Some(version) = poll_probe(&paths.token_file,listen,10_000,version).await? {return Ok(json!({"status":"already-running","pid":pid.pid,"listen":listen["url"],"version":version}));}
        return Err(std::io::Error::other(format!("managed daemon pid {} did not answer initialize",pid.pid)));
    }
    tokio::fs::create_dir_all(&paths.dir).await?;
    let stderr = std::fs::File::create(&paths.stderr_log)?;
    let mut command = tokio::process::Command::new(executable);
    if let Some(agent_dir) = paths.dir.parent() { command.env(maho_core::config::env_agent_dir_var(),agent_dir); }
    command.args(prefix_args).args(["app-server","--listen",listen["url"].as_str().ok_or_else(||std::io::Error::other("Missing listen URL"))?]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr);
    let mut child = command.spawn()?;
    let pid = child.id().ok_or_else(||std::io::Error::other("failed to spawn daemon process"))?;
    let registration = async {
        let identity = wait_for_start_time(u64::from(pid),10_000).await?.ok_or_else(||std::io::Error::other(format!("spawned daemon {pid} started but its process identity stayed unreadable")))?;
        write_sidecar_file_atomic(&paths.pid_file,&format!("{}\n",json!({"pid":pid,"processStartTime":identity}))).await.map_err(std::io::Error::other)?;
        write_sidecar_file_atomic(&paths.settings_file,&format!("{}\n",json!({"listen":listen}))).await.map_err(std::io::Error::other)?;
        tokio::select! {
            probe = poll_probe(&paths.token_file,listen,10_000,version) => if probe?.is_some() {Ok(())} else {Err(std::io::Error::other("spawned daemon did not answer initialize within 10s"))},
            exit = child.wait() => Err(std::io::Error::other(format!("spawned daemon exited with {exit:?} before answering initialize"))),
        }
    }.await;
    if let Err(error) = registration {
        if child.try_wait()?.is_none() {child.kill().await?;child.wait().await?;}
        cleanup_state(&paths.pid_file,&paths.settings_file,listen).await?;
        return Err(error);
    }
    tokio::spawn(async move {if let Err(error) = child.wait().await {eprintln!("app-server daemon reap: {error}");}});
    Ok(json!({"status":"started","pid":pid,"listen":listen["url"]}))
}
pub async fn saved_listen(paths: &DaemonPaths, fallback: &Value) -> std::io::Result<Value> {
    Ok(read_settings(&paths.settings_file).await?.map(|settings|settings["listen"].clone()).unwrap_or_else(||fallback.clone()))
}
pub async fn run_daemon_command(paths: &DaemonPaths, verb: super::cli_args::DaemonVerb, listen: &Value, version: &str, executable: &Path, prefix_args: &[String]) -> std::io::Result<Value> {
    tokio::fs::create_dir_all(&paths.dir).await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let database = loop {
        if tokio::fs::metadata(&paths.lock_file).await.is_ok_and(|metadata|metadata.is_dir()) {return Err(std::io::Error::other(format!("Legacy lock directory is present at {}",paths.lock_file.display())));}
        let path = paths.lock_file.clone();
        let acquired = tokio::task::spawn_blocking(move || {
            let database = rusqlite::Connection::open(path)?;
            database.busy_timeout(std::time::Duration::from_millis(100))?;
            database.execute_batch("BEGIN EXCLUSIVE;")?;
            Ok::<_,rusqlite::Error>(database)
        }).await.map_err(std::io::Error::other)?;
        match acquired {
            Ok(database) => break database,
            Err(rusqlite::Error::SqliteFailure(error,_)) if matches!(error.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked) && tokio::time::Instant::now() < deadline => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            Err(error) => return Err(std::io::Error::other(error)),
        }
    };
    let listen = if verb == super::cli_args::DaemonVerb::Start {listen.clone()} else {saved_listen(paths,listen).await?};
    let result = match verb {
        super::cli_args::DaemonVerb::Start => start_daemon(paths,&listen,version,executable,prefix_args).await,
        super::cli_args::DaemonVerb::Stop => stop_daemon(paths,&listen,version).await,
        super::cli_args::DaemonVerb::Status => status_daemon(paths,&listen,version).await,
        super::cli_args::DaemonVerb::Restart => match stop_daemon(paths,&listen,version).await {Ok(_) => start_daemon(paths,&listen,version,executable,prefix_args).await,Err(error) => Err(error)},
    };
    database.execute_batch("COMMIT;").map_err(std::io::Error::other)?;
    result
}
