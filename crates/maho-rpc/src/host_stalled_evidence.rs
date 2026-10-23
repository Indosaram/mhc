//! Whether a host generation is ALIVE BUT STALLED, from evidence the host itself wrote (senpi `host-stalled-evidence.ts`).
//!
//! `host-alive.json` is a heartbeat refreshed on every healthy loop-lag tick; it stops the moment the loop does.
//! Readers read the files of the generation they act on, never the endpoint's. Nothing here signals anything.
use crate::host_daemon_paths::{
    HOST_DAEMON_DIR_ENV, HostGenerationPaths, generation_paths, host_daemon_directory_paths,
};
use serde_json::{Map, Value};
use std::{collections::HashMap, path::Path};

/// How recent stall evidence keeps an unreachable generation from being replaced.
pub const STALL_REFUSAL_WINDOW_MS_ENV: &str = "SENPI_RPC_STALL_REFUSAL_MS";
pub const DEFAULT_STALL_REFUSAL_WINDOW_MS: u64 = 120_000;

/// The generation a session-holder lease names (senpi `HolderGeneration`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HolderGeneration {
    pub host_instance: Option<String>,
    pub host_daemon_dir: Option<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
pub fn stall_refusal_window_ms(env: &HashMap<String, String>) -> u64 {
    crate::host_lifecycle::parse_idle_exit_ms(
        env.get(STALL_REFUSAL_WINDOW_MS_ENV).map(String::as_str),
    )
    .map_or(DEFAULT_STALL_REFUSAL_WINDOW_MS, |value| value as u64)
}
fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    crate::host_daemon_state::parse_json(std::fs::read_to_string(path).ok().as_deref())
}
fn age_of(at: Option<&Value>, now: i64) -> Option<i64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(at?.as_str()?).ok()?;
    Some(now - parsed.timestamp_millis())
}
/// Whether the generation left a heartbeat older than any stall could explain. No heartbeat file is no evidence.
pub fn generation_heartbeat_stopped_with(
    generation: &HostGenerationPaths,
    now: i64,
    env: &HashMap<String, String>,
) -> bool {
    let record = read_json_object(&generation.alive_file);
    age_of(record.as_ref().and_then(|record| record.get("at")), now)
        .is_some_and(|age| age >= 0 && age as u64 >= stall_refusal_window_ms(env))
}
pub fn generation_heartbeat_stopped(generation: &HostGenerationPaths, now: i64) -> bool {
    generation_heartbeat_stopped_with(generation, now, &std::env::vars().collect())
}

fn is_safe_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Whether the host generation a session-holder lease names has ENDED, for a holder whose pid is alive but whose
/// start time cannot be compared. Its heartbeat stopped, or the lease names an endpoint's daemon directory
/// (`endpoint.json` exists) whose generation directory is gone. This host's own generation never ends.
pub fn holder_generation_ended(holder: &HolderGeneration, env: &HashMap<String, String>) -> bool {
    let Some(instance) = holder.host_instance.as_deref() else {
        return false;
    };
    if !is_safe_instance_id(instance)
        || env
            .get(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
            .map(String::as_str)
            == Some(instance)
    {
        return false;
    }
    let Some(daemon_dir) = holder
        .host_daemon_dir
        .clone()
        .or_else(|| env.get(HOST_DAEMON_DIR_ENV).cloned())
    else {
        return false;
    };
    if !Path::new(&daemon_dir).is_absolute() {
        return false;
    }
    let directory = host_daemon_directory_paths(Path::new(&daemon_dir));
    let generation = generation_paths(&directory, instance);
    if generation_heartbeat_stopped_with(&generation, now_ms(), env) {
        return true;
    }
    holder.host_daemon_dir.is_some() && directory.endpoint_file.exists() && !generation.dir.exists()
}

/// Whether the generation's host CHILD is running, from its own pid record; `None` when unrecorded.
///
/// A live pid whose start time cannot be compared (none recorded, or none readable now) is preserved - a starved
/// probe on a live host must not read as dead - unless the generation's heartbeat has stopped: a killed
/// generation's pid can be handed to an unrelated process, which would otherwise keep the generation alive.
pub fn host_child_alive(
    generation: &HostGenerationPaths,
    mut log: impl FnMut(String),
) -> Option<bool> {
    let record = read_json_object(&generation.child_pid_file)?;
    let pid = record
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)?;
    if !crate::host_reservations::process_is_live(pid) {
        return Some(false);
    }
    let observed = crate::host_reservations::read_process_start_time(pid);
    if !crate::host_reservations::process_is_live(pid) {
        return Some(false);
    }
    if let (Some(recorded), Some(observed)) = (
        record.get("processStartTime").and_then(Value::as_str),
        observed.as_deref(),
    ) {
        return Some(recorded == observed);
    }
    if generation_heartbeat_stopped(generation, now_ms()) {
        log(format!(
            "host child pid {pid} start time unreadable and its heartbeat stopped; releasing generation"
        ));
        return Some(false);
    }
    log(format!(
        "host child pid {pid} start time unreadable; preserving generation"
    ));
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn alive(generation: &HostGenerationPaths, at: &str) {
        std::fs::create_dir_all(&generation.dir).unwrap();
        std::fs::write(
            &generation.alive_file,
            serde_json::json!({"at":at}).to_string(),
        )
        .unwrap();
    }
    fn fresh() -> String {
        chrono::Utc::now().to_rfc3339()
    }
    #[test]
    fn heartbeat_stops_only_past_the_refusal_window_and_no_file_is_no_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let paths = host_daemon_directory_paths(temp.path());
        let generation = generation_paths(&paths, "g");
        let env = HashMap::new();
        assert!(!generation_heartbeat_stopped_with(
            &generation,
            now_ms(),
            &env
        ));
        alive(&generation, &fresh());
        assert!(!generation_heartbeat_stopped_with(
            &generation,
            now_ms(),
            &env
        ));
        alive(&generation, "2000-01-01T00:00:00Z");
        assert!(generation_heartbeat_stopped_with(
            &generation,
            now_ms(),
            &env
        ));
    }
    #[test]
    fn holder_generation_never_ends_for_missing_unsafe_or_own_instances() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_string_lossy().into_owned();
        let env = HashMap::from([(
            crate::protocol_identity::HOST_INSTANCE_ID_ENV.to_owned(),
            "own".to_owned(),
        )]);
        for instance in [None, Some("../x"), Some("own"), Some("")] {
            assert!(!holder_generation_ended(
                &HolderGeneration {
                    host_instance: instance.map(str::to_owned),
                    host_daemon_dir: Some(dir.clone())
                },
                &env
            ));
        }
        assert!(!holder_generation_ended(
            &HolderGeneration {
                host_instance: Some("other".into()),
                host_daemon_dir: Some("relative".into())
            },
            &env
        ));
    }
    #[test]
    fn holder_generation_ends_on_stopped_heartbeat_or_removed_generation_of_a_known_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let paths = host_daemon_directory_paths(temp.path());
        let dir = temp.path().to_string_lossy().into_owned();
        let env = HashMap::new();
        let holder = HolderGeneration {
            host_instance: Some("gone".into()),
            host_daemon_dir: Some(dir.clone()),
        };
        assert!(!holder_generation_ended(&holder, &env));
        std::fs::write(&paths.endpoint_file, "{}").unwrap();
        assert!(holder_generation_ended(&holder, &env));
        assert!(!holder_generation_ended(
            &HolderGeneration {
                host_instance: Some("gone".into()),
                host_daemon_dir: None
            },
            &HashMap::from([(HOST_DAEMON_DIR_ENV.to_owned(), dir)])
        ));
        let generation = generation_paths(&paths, "stale");
        alive(&generation, "2000-01-01T00:00:00Z");
        assert!(holder_generation_ended(
            &HolderGeneration {
                host_instance: Some("stale".into()),
                host_daemon_dir: None
            },
            &HashMap::from([(
                HOST_DAEMON_DIR_ENV.to_owned(),
                temp.path().to_string_lossy().into_owned()
            )])
        ));
    }
    #[test]
    fn child_with_unreadable_start_time_is_released_only_after_its_heartbeat_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let paths = host_daemon_directory_paths(temp.path());
        let generation = generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).unwrap();
        assert_eq!(host_child_alive(&generation, |_| {}), None);
        std::fs::write(
            &generation.child_pid_file,
            serde_json::json!({"pid":std::process::id()}).to_string(),
        )
        .unwrap();
        let mut logs = Vec::new();
        assert_eq!(
            host_child_alive(&generation, |message| logs.push(message)),
            Some(true)
        );
        assert!(logs[0].contains("preserving generation"));
        alive(&generation, "2000-01-01T00:00:00Z");
        let mut logs = Vec::new();
        assert_eq!(
            host_child_alive(&generation, |message| logs.push(message)),
            Some(false)
        );
        assert!(logs[0].contains("heartbeat stopped; releasing generation"));
    }
}
