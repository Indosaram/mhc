pub const INTERNAL_PROCESS_ENV: &str = "__PI_INTERNAL_SPAWN";
pub const MAX_CONTROL_LINE_BYTES: usize = 128 * 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InternalProcessRole { Coordinator, Server, SessionWorker }
pub fn parse_internal_process_role(value: Option<&str>) -> Result<Option<InternalProcessRole>, String> {
    match value { None => Ok(None), Some("coordinator") => Ok(Some(InternalProcessRole::Coordinator)), Some("server") => Ok(Some(InternalProcessRole::Server)), Some("session-worker") => Ok(Some(InternalProcessRole::SessionWorker)), Some(value) => Err(format!("Unsupported internal process role: {value}")) }
}
pub fn encode_control_line(message: &serde_json::Value) -> Result<String, String> {
    let line = format!("{}\n", serde_json::to_string(message).map_err(|e| e.to_string())?); if line.len() > MAX_CONTROL_LINE_BYTES { return Err("Internal control message is too large".to_owned()); } Ok(line)
}
pub fn normalize_agent_dir_lane(env: &mut std::collections::BTreeMap<String, String>) {
    if env.contains_key("PI_CODING_AGENT_DIR") { env.retain(|key, _| key == "PI_CODING_AGENT_DIR" || !key.ends_with("_CODING_AGENT_DIR")); }
}
pub fn consume_internal_process_role(env: &mut std::collections::BTreeMap<String, String>) -> Result<Option<InternalProcessRole>, String> {
    let role = parse_internal_process_role(env.get(INTERNAL_PROCESS_ENV).map(String::as_str))?;
    env.remove(INTERNAL_PROCESS_ENV);
    Ok(role)
}
pub async fn terminate_internal_process(child: &mut tokio::process::Child) -> std::io::Result<()> {
    if child.try_wait()?.is_none() { child.start_kill()?; child.wait().await?; }
    Ok(())
}
pub fn spawn_internal_process(role: InternalProcessRole, args: &[String], cwd: &std::path::Path, env: &std::collections::BTreeMap<String, String>) -> std::io::Result<tokio::process::Child> {
    let mut env = env.clone(); normalize_agent_dir_lane(&mut env);
    env.insert(INTERNAL_PROCESS_ENV.to_owned(), match role { InternalProcessRole::Coordinator => "coordinator", InternalProcessRole::Server => "server", InternalProcessRole::SessionWorker => "session-worker" }.to_owned());
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command.args(args).current_dir(cwd).env_clear().envs(env).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(0x00000008 | 0x00000200 | 0x08000000);
    command.spawn()
}
