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
