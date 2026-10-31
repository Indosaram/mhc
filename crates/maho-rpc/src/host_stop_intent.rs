//! The stop intent and the signal-free drain request (senpi `host-stop-intent.ts`, senpi#2566, senpi#3653).
//!
//! The stop intent is written by whoever is about to signal a host generation, BEFORE the signal, into
//! THAT generation's directory (`stop-intent.json`), so the record written when the generation ends can
//! name who stopped it and why. It is scoped to the target generation because a successor shares the
//! endpoint directory with its predecessor. Writing is best-effort and never blocks the signal.
//!
//! The drain request is the cross-writer record that asks one named generation to drain on every
//! platform.
//!
//! `drain-request.json` is written by whoever asks into the target generation's directory and read by its
//! supervisor; `host-drain-request.json` is the supervisor's relay to its host child. A generation advertises that
//! it reads requests through `drain-watch.json`, naming the supervisor pid that armed the watch. Every request
//! carries a fresh nonce and a watcher delivers each nonce once; an unreadable file is never read as a request, and
//! the reader never removes it.
use serde_json::{Map, Value};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// How often a generation looks for a drain request when it cannot watch the directory.
pub const DRAIN_REQUEST_POLL_MS: u64 = 1_000;
/// The backup poll while a directory watch is armed (events are the primary path).
pub const DRAIN_REQUEST_WATCHED_POLL_MS: u64 = 5_000;

/// Who sent a stop or drain: the pid, its kind, and the generation it acts from when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStopSender {
    pub pid: u32,
    pub kind: String,
    pub generation: Option<String>,
}
const SENDER_KINDS: [&str; 5] = ["ensure", "supervisor", "stop", "successor", "handoff"];
impl HostStopSender {
    fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("pid".into(), self.pid.into());
        object.insert("kind".into(), self.kind.clone().into());
        if let Some(generation) = &self.generation {
            object.insert("generation".into(), generation.clone().into());
        }
        Value::Object(object)
    }
    fn from_value(value: &Value) -> Option<Self> {
        let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
        let kind = value
            .get("kind")?
            .as_str()
            .filter(|kind| SENDER_KINDS.contains(kind))?;
        Some(Self {
            pid,
            kind: kind.into(),
            generation: value
                .get("generation")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDrainRequest {
    pub sender: HostStopSender,
    pub reason: String,
    pub at: String,
    /** Distinguishes one request from the next. */
    pub nonce: String,
}

fn write_json_atomic(path: &Path, content: &Value) -> std::io::Result<()> {
    let temporary = PathBuf::from(format!(
        "{}.{}.{}.tmp",
        path.display(),
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let result = (|| {
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        writeln!(options.open(&temporary)?, "{content}")?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    crate::host_daemon_state::parse_json(std::fs::read_to_string(path).ok().as_deref())
}
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// An intent older than this describes some earlier stop, not the death being recorded now.
pub const STOP_INTENT_MAX_AGE_MS: i64 = 120_000;
/// Shared with callers so they never kill a supervisor while it is reaping its child.
pub const CHILD_STOP_TIMEOUT_MS: u64 = 5_000;
pub const CHILD_KILL_EXIT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStopIntent {
    pub sender: HostStopSender,
    /// Present once a second sender acted on the same stop (the supervisor after an outer caller).
    pub chain: Option<Vec<HostStopSender>>,
    pub target_pid: u32,
    pub reason: String,
    pub signal: String,
    pub at: String,
}
impl HostStopIntent {
    fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("sender".into(), self.sender.to_value());
        if let Some(chain) = &self.chain {
            object.insert(
                "chain".into(),
                Value::Array(chain.iter().map(HostStopSender::to_value).collect()),
            );
        }
        object.insert("targetPid".into(), self.target_pid.into());
        object.insert("reason".into(), self.reason.clone().into());
        object.insert("signal".into(), self.signal.clone().into());
        object.insert("at".into(), self.at.clone().into());
        Value::Object(object)
    }
}
fn parse_stop_intent(raw: &Map<String, Value>) -> Option<HostStopIntent> {
    let sender = HostStopSender::from_value(raw.get("sender")?)?;
    let chain = match raw.get("chain") {
        Some(Value::Array(entries)) => Some(
            entries
                .iter()
                .map(HostStopSender::from_value)
                .collect::<Option<Vec<_>>>()?,
        ),
        _ => None,
    };
    Some(HostStopIntent {
        sender,
        chain,
        target_pid: raw
            .get("targetPid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .unwrap_or(0),
        reason: raw.get("reason")?.as_str()?.to_owned(),
        signal: raw
            .get("signal")
            .and_then(Value::as_str)
            .unwrap_or("SIGTERM")
            .to_owned(),
        at: raw.get("at")?.as_str()?.to_owned(),
    })
}
fn age_of(at: Option<&Value>, now_ms: i64) -> Option<i64> {
    let at = chrono::DateTime::parse_from_rfc3339(at?.as_str()?).ok()?;
    Some(now_ms - at.timestamp_millis())
}

/// Best-effort: a failed write is logged and never blocks the signal it precedes.
pub fn write_stop_intent(
    generation: &crate::host_daemon_paths::HostGenerationPaths,
    intent: &HostStopIntent,
    mut log: impl FnMut(String),
) {
    if let Err(cause) = write_json_atomic(&generation.stop_intent_file, &intent.to_value()) {
        log(format!(
            "could not record the stop intent for {}: {cause}",
            generation.dir.display()
        ));
    }
}
/// This generation's own intent while it is fresh; a stale or unparsable one is removed and reads as none.
pub fn read_stop_intent(
    generation: &crate::host_daemon_paths::HostGenerationPaths,
    now_ms: i64,
    max_age_ms: Option<i64>,
) -> Option<HostStopIntent> {
    let raw = read_json_object(&generation.stop_intent_file)?;
    let intent = parse_stop_intent(&raw);
    let age = age_of(raw.get("at"), now_ms);
    if let (Some(intent), Some(age)) = (intent, age)
        && age <= max_age_ms.unwrap_or(STOP_INTENT_MAX_AGE_MS)
    {
        return Some(intent);
    }
    clear_stop_intent(generation);
    None
}
/// Read-and-clear: the record that cites the intent is the intent's only consumer.
pub fn consume_stop_intent(
    generation: &crate::host_daemon_paths::HostGenerationPaths,
    now_ms: i64,
) -> Option<HostStopIntent> {
    let intent = read_stop_intent(generation, now_ms, None);
    if intent.is_some() {
        clear_stop_intent(generation);
    }
    intent
}
pub fn clear_stop_intent(generation: &crate::host_daemon_paths::HostGenerationPaths) {
    let _ = std::fs::remove_file(&generation.stop_intent_file);
}
/// The supervisor's own intent before it signals its child. An OUTER intent for this generation is never
/// overwritten: the supervisor appends itself to its chain and its reason to the outer reason.
pub fn layered_supervisor_intent(
    outer: Option<&HostStopIntent>,
    supervisor: HostStopSender,
    target_pid: u32,
    reason: &str,
    at: &str,
) -> HostStopIntent {
    let Some(outer) = outer else {
        return HostStopIntent {
            sender: supervisor,
            chain: None,
            target_pid,
            reason: reason.to_owned(),
            signal: "SIGTERM".into(),
            at: at.to_owned(),
        };
    };
    let mut chain = outer
        .chain
        .clone()
        .unwrap_or_else(|| vec![outer.sender.clone()]);
    chain.push(supervisor);
    HostStopIntent {
        sender: outer.sender.clone(),
        chain: Some(chain),
        target_pid,
        reason: format!("{} -> {reason}", outer.reason),
        signal: "SIGTERM".into(),
        at: at.to_owned(),
    }
}

/// Records a drain request for whoever polls `file`. Atomic: a reader sees no request or a whole one.
pub fn write_drain_request(
    file: &Path,
    sender: &HostStopSender,
    reason: &str,
) -> std::io::Result<()> {
    write_json_atomic(
        file,
        &serde_json::json!({"sender":sender.to_value(),"reason":reason,"at":now_iso(),"nonce":uuid::Uuid::new_v4().to_string()}),
    )
}
/// The request at `file`, or `None` when it is absent or does not parse as one. Never removes the file.
pub fn read_drain_request(file: &Path) -> Option<HostDrainRequest> {
    let raw = read_json_object(file)?;
    Some(HostDrainRequest {
        sender: HostStopSender::from_value(raw.get("sender")?)?,
        reason: raw.get("reason")?.as_str()?.into(),
        at: raw.get("at")?.as_str()?.into(),
        nonce: raw.get("nonce")?.as_str()?.into(),
    })
}

/// Handle for an armed drain watch. Dropping it or calling `stop` ends the watch.
pub struct DrainRequestWatch {
    task: tokio::task::JoinHandle<()>,
}
impl DrainRequestWatch {
    pub fn stop(self) {
        self.task.abort();
    }
}
impl Drop for DrainRequestWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Calls `on_request` for every distinct request that appears at `file`, including one already there when the
/// watch arms. The parent directory is watched with the platform file-notification API; the backup poll
/// (`poll_ms`, default `DRAIN_REQUEST_WATCHED_POLL_MS`) covers a missed event, and without a watch (no directory
/// yet, or the watcher cannot arm) polling alone runs at `DRAIN_REQUEST_POLL_MS`. The task never keeps the runtime alive.
pub fn watch_drain_request(
    file: PathBuf,
    mut on_request: impl FnMut(HostDrainRequest) + Send + 'static,
    poll_ms: Option<u64>,
) -> DrainRequestWatch {
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let watcher = arm_directory_watch(&file, wake_tx);
    let default_ms = if watcher.is_some() {
        DRAIN_REQUEST_WATCHED_POLL_MS
    } else {
        DRAIN_REQUEST_POLL_MS
    };
    let period = std::time::Duration::from_millis(poll_ms.unwrap_or(default_ms).max(1));
    let task = tokio::spawn(async move {
        let _watcher = watcher;
        let mut last_nonce: Option<String> = None;
        loop {
            if let Some(request) = read_drain_request(&file)
                && last_nonce.as_deref() != Some(request.nonce.as_str())
            {
                last_nonce = Some(request.nonce.clone());
                on_request(request);
            }
            tokio::select! {
                _=tokio::time::sleep(period)=>{},
                _=wake_rx.recv()=>{},
            }
        }
    });
    DrainRequestWatch { task }
}
/// Watches the request file's directory; every event wakes the reader. `None` when the directory cannot be watched.
fn arm_directory_watch(
    file: &Path,
    wake: tokio::sync::mpsc::UnboundedSender<()>,
) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let directory = file.parent().filter(|directory| directory.is_dir())?;
    let mut watcher = notify::recommended_watcher(move |_event: notify::Result<notify::Event>| {
        let _ = wake.send(());
    })
    .ok()?;
    watcher
        .watch(directory, notify::RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

/// What a supervisor publishes once its watch is armed: the pid an asker must prove before asking.
pub fn write_drain_watch(file: &Path, pid: u32) -> std::io::Result<()> {
    write_json_atomic(file, &serde_json::json!({"pid":pid,"at":now_iso()}))
}
/// The advertised watcher pid, or `None`: a generation without the marker cannot be asked this way.
pub fn read_drain_watch(file: &Path) -> Option<u32> {
    let pid = read_json_object(file)?
        .get("pid")?
        .as_u64()
        .filter(|pid| *pid > 0)?;
    u32::try_from(pid).ok()
}
/// The relay file a SUPERVISED host child polls, derived from what its supervisor told it at spawn (daemon
/// directory + instance id). A host started bare has no supervisor to relay anything.
pub fn host_child_drain_request_file(
    env: &std::collections::HashMap<String, String>,
) -> Option<PathBuf> {
    let daemon_dir = env
        .get(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV)
        .filter(|value| !value.is_empty())?;
    let instance_id = env
        .get(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
        .filter(|value| !value.is_empty())?;
    Some(
        crate::host_daemon_paths::generation_paths(
            &crate::host_daemon_paths::host_daemon_directory_paths(Path::new(daemon_dir)),
            instance_id,
        )
        .child_drain_request_file,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sender() -> HostStopSender {
        HostStopSender {
            pid: 7,
            kind: "stop".into(),
            generation: Some("g".into()),
        }
    }
    fn generation(dir: &Path) -> crate::host_daemon_paths::HostGenerationPaths {
        crate::host_daemon_paths::generation_paths(
            &crate::host_daemon_paths::host_daemon_directory_paths(dir),
            "g1",
        )
    }
    #[test]
    fn stop_intent_round_trips_while_fresh_and_is_consumed_once() {
        let temp = tempfile::tempdir().unwrap();
        let generation = generation(temp.path());
        std::fs::create_dir_all(&generation.dir).unwrap();
        let at = "2026-10-30T00:00:00.000Z";
        let now = chrono::DateTime::parse_from_rfc3339(at)
            .unwrap()
            .timestamp_millis();
        let intent = layered_supervisor_intent(None, sender(), 42, "idle", at);
        write_stop_intent(&generation, &intent, |_| panic!("write failed"));
        assert_eq!(
            read_stop_intent(&generation, now + 1_000, None),
            Some(intent.clone())
        );
        assert_eq!(consume_stop_intent(&generation, now + 1_000), Some(intent));
        assert_eq!(read_stop_intent(&generation, now + 1_000, None), None);
    }
    #[test]
    fn a_stale_stop_intent_is_removed_and_reads_as_none() {
        let temp = tempfile::tempdir().unwrap();
        let generation = generation(temp.path());
        std::fs::create_dir_all(&generation.dir).unwrap();
        let at = "2026-10-30T00:00:00.000Z";
        let now = chrono::DateTime::parse_from_rfc3339(at)
            .unwrap()
            .timestamp_millis();
        write_stop_intent(
            &generation,
            &layered_supervisor_intent(None, sender(), 1, "x", at),
            |_| {},
        );
        assert_eq!(
            read_stop_intent(&generation, now + STOP_INTENT_MAX_AGE_MS + 1, None),
            None
        );
        assert!(!generation.stop_intent_file.exists());
    }
    #[test]
    fn the_supervisor_layers_onto_an_outer_intent() {
        let outer = layered_supervisor_intent(None, sender(), 1, "operator", "t0");
        let supervisor = HostStopSender {
            pid: 9,
            kind: "supervisor".into(),
            generation: None,
        };
        let layered =
            layered_supervisor_intent(Some(&outer), supervisor.clone(), 2, "shutdown", "t1");
        assert_eq!(layered.sender, sender());
        assert_eq!(layered.chain, Some(vec![sender(), supervisor]));
        assert_eq!(
            (
                layered.reason.as_str(),
                layered.target_pid,
                layered.at.as_str()
            ),
            ("operator -> shutdown", 2, "t1")
        );
    }
    #[test]
    fn drain_request_round_trips_and_is_never_removed_by_reading() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("drain-request.json");
        assert!(read_drain_request(&file).is_none());
        write_drain_request(&file, &sender(), "operator").unwrap();
        let first = read_drain_request(&file).unwrap();
        assert_eq!(first.sender, sender());
        assert_eq!(first.reason, "operator");
        assert_eq!(read_drain_request(&file).unwrap().nonce, first.nonce);
        write_drain_request(&file, &sender(), "again").unwrap();
        assert_ne!(read_drain_request(&file).unwrap().nonce, first.nonce);
        std::fs::write(&file, "{not json").unwrap();
        assert!(read_drain_request(&file).is_none());
        assert!(file.exists());
    }
    #[test]
    fn drain_watch_needs_a_positive_integer_pid() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("drain-watch.json");
        assert!(read_drain_watch(&file).is_none());
        write_drain_watch(&file, 4242).unwrap();
        assert_eq!(read_drain_watch(&file), Some(4242));
        std::fs::write(&file, r#"{"pid":0}"#).unwrap();
        assert!(read_drain_watch(&file).is_none());
        std::fs::write(&file, r#"{"pid":1.5}"#).unwrap();
        assert!(read_drain_watch(&file).is_none());
    }
    #[test]
    fn child_relay_file_needs_daemon_dir_and_instance_id() {
        let env = std::collections::HashMap::from([
            (
                crate::host_daemon_paths::HOST_DAEMON_DIR_ENV.to_owned(),
                "/d".to_owned(),
            ),
            (
                crate::protocol_identity::HOST_INSTANCE_ID_ENV.to_owned(),
                "i".to_owned(),
            ),
        ]);
        assert_eq!(
            host_child_drain_request_file(&env),
            Some(PathBuf::from("/d/generations/i/host-drain-request.json"))
        );
        assert!(host_child_drain_request_file(&std::collections::HashMap::new()).is_none());
    }
    #[tokio::test]
    async fn watch_delivers_a_pre_existing_request_once_and_then_each_new_nonce() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("drain-request.json");
        write_drain_request(&file, &sender(), "first").unwrap();
        let (sender_tx, mut received) = tokio::sync::mpsc::unbounded_channel();
        let watch = watch_drain_request(
            file.clone(),
            move |request| {
                let _ = sender_tx.send(request.reason);
            },
            Some(5),
        );
        let wait = std::time::Duration::from_secs(5);
        assert_eq!(
            tokio::time::timeout(wait, received.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        write_drain_request(&file, &sender(), "second").unwrap();
        assert_eq!(
            tokio::time::timeout(wait, received.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("second")
        );
        assert!(received.try_recv().is_err());
        watch.stop();
    }
}
