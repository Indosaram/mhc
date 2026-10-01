use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonPidFile {
    pub pid: u64,
    pub process_start_time: Option<String>,
}
pub fn parse_daemon_pid_file(text: &str) -> Option<DaemonPidFile> {
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let pid = object.get("pid")?.as_u64().filter(|pid| *pid > 0)?;
    let identity = match object.get("processStartTime")? {
        Value::Null => None,
        Value::String(identity) if !identity.trim().is_empty() => Some(identity.clone()),
        _ => return None,
    };
    Some(DaemonPidFile { pid, process_start_time: identity })
}
