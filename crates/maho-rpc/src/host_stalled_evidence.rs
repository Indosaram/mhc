//! Whether a host generation is ALIVE BUT STALLED, from evidence the host itself wrote (senpi `host-stalled-evidence.ts`).
//!
//! The host's loop-lag watchdog leaves two files in its own generation directory: `host-stalled.json` for the
//! newest stall past the error threshold (written after the stall ends, so it proves a RECURRING stall), and
//! `host-alive.json`, a heartbeat refreshed on every healthy tick (which stops the moment the loop does, so it
//! proves a stall IN PROGRESS). Readers read the files of the generation they act on, never the endpoint's,
//! because a successor and its predecessor share the endpoint directory during a handoff. Nothing here signals
//! anything.
use crate::host_daemon_paths::{
    HOST_DAEMON_DIR_ENV, HostGenerationPaths, generation_paths, host_daemon_directory_paths,
};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// How recent stall evidence keeps an unreachable generation from being replaced.
pub const STALL_REFUSAL_WINDOW_MS_ENV: &str = "SENPI_RPC_STALL_REFUSAL_MS";
pub const DEFAULT_STALL_REFUSAL_WINDOW_MS: u64 = 120_000;
/// How recent stall evidence keeps a graceful stop waiting instead of escalating.
pub const STOP_STALL_EVIDENCE_MAX_AGE_MS: u64 = 60_000;
/// The longest a graceful stop waits for a stalled child before it escalates to SIGKILL.
pub const CHILD_STALLED_STOP_MAX_MS_ENV: &str = "SENPI_RPC_CHILD_STALLED_STOP_MAX_MS";
pub const DEFAULT_CHILD_STALLED_STOP_MAX_MS: u64 = 60_000;

pub fn child_stalled_stop_max_ms(env: &HashMap<String, String>) -> u64 {
    crate::host_lifecycle::parse_idle_exit_ms(
        env.get(CHILD_STALLED_STOP_MAX_MS_ENV).map(String::as_str),
    )
    .map_or(DEFAULT_CHILD_STALLED_STOP_MAX_MS, |value| value as u64)
}

/// The newest stall the host's loop-lag watchdog measured (senpi `HostStallEvidence`).
#[derive(Debug, Clone, PartialEq)]
pub struct HostStallEvidence {
    pub at: String,
    pub drift_ms: f64,
    pub process_cpu_ms: f64,
    pub heap_delta_mb: f64,
    pub session_id: Option<String>,
    pub tool: Option<String>,
}
impl HostStallEvidence {
    fn to_value(&self) -> Value {
        let mut value = serde_json::json!({
            "at": self.at,
            "driftMs": self.drift_ms,
            "processCpuMs": self.process_cpu_ms,
            "heapDeltaMb": self.heap_delta_mb,
        });
        if let Some(session_id) = &self.session_id {
            value["sessionId"] = session_id.clone().into();
        }
        if let Some(tool) = &self.tool {
            value["tool"] = tool.clone().into();
        }
        value
    }
}

/// `generations/<instanceId>/` of the host this process IS, when a supervisor launched it; a plain
/// `--mode rpc` run has neither variable and writes no evidence at all.
pub fn own_generation_dir(env: &HashMap<String, String>) -> Option<PathBuf> {
    let daemon_dir = env
        .get(HOST_DAEMON_DIR_ENV)
        .filter(|value| !value.is_empty())?;
    let instance_id = env
        .get(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
        .filter(|value| !value.is_empty())?;
    Some(Path::new(daemon_dir).join("generations").join(instance_id))
}

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
/// senpi `writeJsonAtomic`: by rename, so a reader sees the previous content or the new one, never a torn file.
fn write_json_atomic(path: &Path, content: &Value) -> std::io::Result<()> {
    let temporary = PathBuf::from(format!(
        "{}.{}.{}.tmp",
        path.display(),
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        std::io::Write::write_all(
            &mut options.open(&temporary)?,
            format!("{content}\n").as_bytes(),
        )?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Best-effort for the caller: a generation directory that is gone (released) or read-only simply gets no evidence.
pub fn write_stall_evidence(
    generation_dir: &Path,
    evidence: &HostStallEvidence,
) -> std::io::Result<()> {
    write_json_atomic(
        &generation_dir.join("host-stalled.json"),
        &evidence.to_value(),
    )
}

pub fn write_heartbeat(generation_dir: &Path, at: &str) -> std::io::Result<()> {
    write_json_atomic(
        &generation_dir.join("host-alive.json"),
        &serde_json::json!({ "at": at }),
    )
}

pub fn read_stall_evidence(generation: &HostGenerationPaths) -> Option<HostStallEvidence> {
    let raw = read_json_object(&generation.stalled_file)?;
    let at = raw.get("at")?.as_str()?.to_owned();
    let drift_ms = raw.get("driftMs")?.as_f64()?;
    let number = |key: &str| raw.get(key).and_then(Value::as_f64).unwrap_or(0.);
    let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(HostStallEvidence {
        at,
        drift_ms,
        process_cpu_ms: number("processCpuMs"),
        heap_delta_mb: number("heapDeltaMb"),
        session_id: text("sessionId"),
        tool: text("tool"),
    })
}

pub fn recent_stall(
    generation: &HostGenerationPaths,
    now: i64,
    max_age_ms: i64,
) -> Option<HostStallEvidence> {
    let evidence = read_stall_evidence(generation)?;
    let age = age_of(Some(&Value::String(evidence.at.clone())), now)?;
    (age <= max_age_ms).then_some(evidence)
}

/// What [`host_loop_stalled`] judges by (senpi's `{ now, errorMs, windowMs }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopStallWindow {
    pub now: i64,
    pub error_ms: i64,
    pub window_ms: i64,
}

/// Stalled = a recent measured stall, OR a heartbeat that stopped more than `error_ms` ago yet within the window
/// (older than the window it is a host that died without cleaning up, not one that is stuck). The caller pairs
/// this with its own proof that the process is still alive.
pub fn host_loop_stalled(generation: &HostGenerationPaths, options: LoopStallWindow) -> bool {
    if recent_stall(generation, options.now, options.window_ms).is_some() {
        return true;
    }
    let record = read_json_object(&generation.alive_file);
    age_of(
        record.as_ref().and_then(|record| record.get("at")),
        options.now,
    )
    .is_some_and(|age| age > options.error_ms && age < options.window_ms)
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
    // An exited, unreaped child (state Z) is absent, read right after the identity snapshot (senpi#4119).
    if !crate::host_reservations::process_is_running(pid) {
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

/// The supervisor's report that it is waiting out a stalled child before escalating; a caller with a deadline on
/// that supervisor extends it while the report is fresh (`phase` is always `waiting_stalled_child`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopProgress {
    pub at: String,
    pub until_at: String,
}
pub const STOP_PROGRESS_PHASE: &str = "waiting_stalled_child";

pub fn write_stop_progress(
    generation: &HostGenerationPaths,
    progress: &StopProgress,
) -> std::io::Result<()> {
    write_json_atomic(
        &generation.stop_progress_file,
        &serde_json::json!({"at": progress.at, "phase": STOP_PROGRESS_PHASE, "untilAt": progress.until_at}),
    )
}

pub fn clear_stop_progress(generation: &HostGenerationPaths) {
    let _ = std::fs::remove_file(&generation.stop_progress_file);
}

/// The `untilAt` (epoch ms) of a fresh, still-running stall wait, or `None`.
pub fn active_stop_progress(
    generation: &HostGenerationPaths,
    now: i64,
    fresh_ms: i64,
) -> Option<i64> {
    let raw = read_json_object(&generation.stop_progress_file)?;
    let age = age_of(raw.get("at"), now)?;
    let until_at = chrono::DateTime::parse_from_rfc3339(raw.get("untilAt")?.as_str()?)
        .ok()?
        .timestamp_millis();
    (age <= fresh_ms && until_at > now).then_some(until_at)
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
    fn iso(ms: i64) -> String {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
    #[test]
    fn child_stalled_stop_max_reads_its_env_override() {
        assert_eq!(
            child_stalled_stop_max_ms(&HashMap::new()),
            DEFAULT_CHILD_STALLED_STOP_MAX_MS
        );
        let env = HashMap::from([(CHILD_STALLED_STOP_MAX_MS_ENV.to_owned(), "1500".to_owned())]);
        assert_eq!(child_stalled_stop_max_ms(&env), 1500);
    }
    #[test]
    fn own_generation_dir_needs_both_supervisor_variables() {
        assert_eq!(own_generation_dir(&HashMap::new()), None);
        let env = HashMap::from([
            (HOST_DAEMON_DIR_ENV.to_owned(), "/d".to_owned()),
            (
                crate::protocol_identity::HOST_INSTANCE_ID_ENV.to_owned(),
                "i".to_owned(),
            ),
        ]);
        assert_eq!(
            own_generation_dir(&env),
            Some(PathBuf::from("/d/generations/i"))
        );
    }
    #[test]
    fn stall_evidence_round_trips_and_ages_out_of_the_window() {
        let temp = tempfile::tempdir().unwrap();
        let paths = host_daemon_directory_paths(temp.path());
        let generation = generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).unwrap();
        let now = 1_700_000_000_000;
        assert_eq!(read_stall_evidence(&generation), None);
        let evidence = HostStallEvidence {
            at: iso(now - 1_000),
            drift_ms: 6_000.,
            process_cpu_ms: 0.,
            heap_delta_mb: 0.,
            session_id: Some("s".into()),
            tool: None,
        };
        write_stall_evidence(&generation.dir, &evidence).unwrap();
        assert_eq!(read_stall_evidence(&generation), Some(evidence.clone()));
        assert_eq!(recent_stall(&generation, now, 2_000), Some(evidence));
        assert_eq!(recent_stall(&generation, now, 500), None);
        let window = LoopStallWindow {
            now,
            error_ms: 5_000,
            window_ms: 2_000,
        };
        assert!(host_loop_stalled(&generation, window));
        assert!(!host_loop_stalled(
            &generation,
            LoopStallWindow {
                window_ms: 500,
                ..window
            }
        ));
    }
    #[test]
    fn a_stopped_heartbeat_is_a_stall_only_between_the_threshold_and_the_window() {
        let temp = tempfile::tempdir().unwrap();
        let paths = host_daemon_directory_paths(temp.path());
        let generation = generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).unwrap();
        let now = 1_700_000_000_000;
        let window = LoopStallWindow {
            now,
            error_ms: 5_000,
            window_ms: 120_000,
        };
        assert!(
            !host_loop_stalled(&generation, window),
            "no file is no evidence"
        );
        for (age, stalled) in [(1_000, false), (10_000, true), (200_000, false)] {
            write_heartbeat(&generation.dir, &iso(now - age)).unwrap();
            assert_eq!(host_loop_stalled(&generation, window), stalled, "age {age}");
        }
    }
    #[test]
    fn stop_progress_counts_only_while_fresh_and_unexpired() {
        let temp = tempfile::tempdir().unwrap();
        let paths = host_daemon_directory_paths(temp.path());
        let generation = generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).unwrap();
        let now = 1_700_000_000_000;
        assert_eq!(active_stop_progress(&generation, now, 10_000), None);
        write_stop_progress(
            &generation,
            &StopProgress {
                at: iso(now - 1_000),
                until_at: iso(now + 5_000),
            },
        )
        .unwrap();
        assert_eq!(
            active_stop_progress(&generation, now, 10_000),
            Some(now + 5_000)
        );
        assert_eq!(
            active_stop_progress(&generation, now, 500),
            None,
            "stale report"
        );
        assert_eq!(
            active_stop_progress(&generation, now + 6_000, 10_000),
            None,
            "expired"
        );
        clear_stop_progress(&generation);
        assert_eq!(active_stop_progress(&generation, now, 10_000), None);
        clear_stop_progress(&generation);
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
