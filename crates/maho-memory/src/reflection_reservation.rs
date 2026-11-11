//! Persisting the reflection reservation between runs.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-reflection-reservation.md`. The reservation is two files:
//! `active.lock` (the run holding reflection) and `pending.json` (the one run waiting). Both are read by
//! a process that may not be the one that wrote them, so a half-written file is not a theoretical
//! concern — a torn `pending.json` would make the next run believe nothing is queued, or crash it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::reflection_machine::{
    JournalSnapshot, ReflectionOutcome, ReflectionRequest, ReservedRun, TriggerConfig,
    complete_transition, reserve_transition,
};

pub use crate::reflection_machine::ReservationState;

#[derive(Debug)]
pub enum ReservationStateError {
    Io(std::io::Error),
    Corrupt { path: PathBuf, detail: String },
}

impl std::fmt::Display for ReservationStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "reservation store io error: {error}"),
            Self::Corrupt { path, detail } => write!(
                formatter,
                "Invalid reflection reservation {}: {detail}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ReservationStateError {}

impl From<std::io::Error> for ReservationStateError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationPaths {
    pub active: PathBuf,
    pub pending: PathBuf,
}

impl ReservationPaths {
    pub fn under(reflection_dir: &Path) -> Self {
        Self {
            active: reflection_dir.join("active.lock"),
            pending: reflection_dir.join("pending.json"),
        }
    }
}

/// Read both files.
///
/// A missing file means "nothing here", not an error — that is the state of every identity that has
/// never reflected. A file that exists but does not parse is reported as corrupt and is never treated as
/// absent: silently discarding it would let a damaged store look idle and let a second run start on top
/// of a reservation that is still real.
pub fn read_state(paths: &ReservationPaths) -> Result<ReservationState, ReservationStateError> {
    let active = read_run(&paths.active)?;
    let pending = read_run(&paths.pending)?;
    // A run cannot be both. When the same id is in both files the active copy wins and the pending copy
    // is dropped, because the pending entry is the queue and re-queuing the run that is already running
    // would make it run twice.
    let pending = match (active.as_ref(), pending) {
        (Some(active), Some(pending)) if active.run_id == pending.run_id => None,
        (_, pending) => pending,
    };
    Ok(ReservationState { active, pending })
}

pub fn write_state(
    paths: &ReservationPaths,
    state: &ReservationState,
) -> Result<(), ReservationStateError> {
    write_optional_run(&paths.active, state.active.as_ref())?;
    write_optional_run(&paths.pending, state.pending.as_ref())?;
    Ok(())
}

fn read_run(path: &Path) -> Result<Option<ReservedRun>, ReservationStateError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ReservationStateError::Io(error)),
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| ReservationStateError::Corrupt {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })
}

fn write_optional_run(path: &Path, run: Option<&ReservedRun>) -> Result<(), ReservationStateError> {
    match run {
        Some(run) => write_json_atomic(path, run),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ReservationStateError::Io(error)),
        },
    }
}

/// Write through a temp file and rename, so a crash mid-write leaves the previous file intact.
///
/// The temp name carries a random suffix: two writers racing on the same reservation must not share a
/// temp path, or one truncates the other's half-written file and the rename publishes garbage.
/// `sync_all` before the rename is what makes the rename a real boundary — without it the new name can be
/// visible while its contents are still not on disk, and a power loss restores an empty file.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), ReservationStateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_name = format!(
        "{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run"),
        crate::random_ids::random_id()
    );
    let temp_path = path.with_file_name(temp_name);
    let mut file = std::fs::File::create(&temp_path)?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    use std::io::Write;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

/// Identity of the process that reserved a run, recorded on the run itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionLauncherIdentity {
    pub pid: u32,
    pub hostname: String,
    pub process_start: Option<String>,
}

/// What a reservation attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationResult {
    pub status: String,
    pub run: ReservedRun,
}

/// What completing a run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResult {
    pub outcome: ReflectionOutcome,
    pub launch: Option<ReservedRun>,
}

#[derive(Debug)]
pub enum ReservationError {
    Io(std::io::Error),
    State(ReservationStateError),
    CorruptState(String),
    TransitionFailed(String),
}

impl std::fmt::Display for ReservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "reservation store io error: {error}"),
            Self::State(error) => error.fmt(formatter),
            Self::CorruptState(message) => {
                write!(formatter, "corrupt reservation state: {message}")
            }
            Self::TransitionFailed(message) => write!(formatter, "transition failed: {message}"),
        }
    }
}

impl std::error::Error for ReservationError {}

impl From<std::io::Error> for ReservationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ReservationStateError> for ReservationError {
    fn from(error: ReservationStateError) -> Self {
        Self::State(error)
    }
}

/// Holds the reservation without taking an OS lock.
///
/// Callers that must exclude other processes wrap [`ReservationStore::reserve`] and
/// [`ReservationStore::complete`] in `lock_acquire::with_lock`. Keeping the lock out of this type is what
/// lets the transitions be tested on a plain directory.
pub struct ReservationStore {
    paths: ReservationPaths,
    config: TriggerConfig,
    now_iso: Box<dyn Fn() -> String + Send + Sync>,
    create_run_id: Box<dyn Fn() -> String + Send + Sync>,
    launcher_identity: Box<dyn Fn() -> ReflectionLauncherIdentity + Send + Sync>,
}

impl ReservationStore {
    pub fn new(paths: ReservationPaths, config: TriggerConfig) -> Self {
        Self {
            paths,
            config,
            now_iso: Box::new(|| {
                chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string()
            }),
            create_run_id: Box::new(crate::random_ids::random_uuid),
            launcher_identity: Box::new(default_launcher_identity),
        }
    }

    pub fn with_now_iso(mut self, now: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.now_iso = Box::new(now);
        self
    }

    pub fn with_run_id(mut self, create: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.create_run_id = Box::new(create);
        self
    }

    pub fn with_launcher_identity(
        mut self,
        launcher: impl Fn() -> ReflectionLauncherIdentity + Send + Sync + 'static,
    ) -> Self {
        self.launcher_identity = Box::new(launcher);
        self
    }

    pub fn read_state(&self) -> Result<ReservationState, ReservationError> {
        Ok(read_state(&self.paths)?)
    }

    pub fn reserve(
        &self,
        request: ReflectionRequest,
    ) -> Result<ReservationResult, ReservationError> {
        let run_id = (self.create_run_id)();
        let current = self.read_state()?;
        let (mut state, status) = reserve_transition(current, request, run_id);
        if status == "active"
            && let Some(active) = &mut state.active
        {
            self.stamp_launch_owner(active);
        }
        write_state(&self.paths, &state)?;
        let run = if status == "active" {
            state
                .active
                .ok_or_else(|| ReservationError::TransitionFailed("No active run".to_string()))?
        } else {
            state
                .pending
                .ok_or_else(|| ReservationError::TransitionFailed("No pending run".to_string()))?
        };
        Ok(ReservationResult {
            status: status.to_string(),
            run,
        })
    }

    /// Complete the active run, then promote a pending run when its trigger still holds.
    ///
    /// `journals` decides whether the pending run launches, so a caller that cannot supply the snapshot for
    /// every conversation must not call this: an empty map silently drops the pending run instead of
    /// reporting that it could not be evaluated.
    pub fn complete(
        &self,
        run_id: &str,
        outcome: ReflectionOutcome,
        journals: &BTreeMap<String, JournalSnapshot>,
    ) -> Result<CompletionResult, ReservationError> {
        let current = self.read_state()?;
        let transition = complete_transition(current, run_id, outcome, journals, &self.config)
            .map_err(|error| ReservationError::TransitionFailed(error.to_string()))?;
        write_state(&self.paths, &transition.state)?;
        Ok(CompletionResult {
            outcome,
            launch: transition.launch,
        })
    }

    fn stamp_launch_owner(&self, run: &mut ReservedRun) {
        let launcher = (self.launcher_identity)();
        run.reserved_at = Some((self.now_iso)());
        run.launcher_pid = Some(launcher.pid);
        run.launcher_hostname = Some(launcher.hostname);
        run.launcher_process_start = launcher.process_start;
    }
}

fn default_launcher_identity() -> ReflectionLauncherIdentity {
    let pid = std::process::id();
    ReflectionLauncherIdentity {
        pid,
        hostname: crate::host_identity::hostname(),
        process_start: {
            use crate::lock_record::LockHost;
            crate::host_identity::SystemLockHost.process_start(pid)
        },
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DreamState {
    pub last_dream_at: String,
    pub last_run_id: String,
}

pub fn write_dream_state(
    runtime: &Path,
    now_iso: &str,
    run_id: &str,
) -> Result<(), ReservationStateError> {
    let state = DreamState {
        last_dream_at: now_iso.to_string(),
        last_run_id: run_id.to_string(),
    };
    write_json_atomic(&runtime.join("dream").join("state.json"), &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflection_machine::{ReflectionRequest, ReflectionTrigger};

    fn run(id: &str) -> ReservedRun {
        ReservedRun {
            run_id: id.to_string(),
            request: ReflectionRequest {
                trigger: ReflectionTrigger::Manual,
                origin: None,
                conversation_ids: vec!["conv".to_string()],
                snapshots: Vec::new(),
                focus: None,
                recent_n: None,
                target_doc: None,
            },
            reserved_at: None,
            launcher_pid: None,
            launcher_hostname: None,
            launcher_process_start: None,
        }
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "maho-resv-{name}-{}",
            crate::random_ids::random_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_fresh_directory_reads_as_idle() {
        let dir = tempdir("fresh");
        let paths = ReservationPaths::under(&dir);
        let state = read_state(&paths).unwrap();
        assert_eq!(state, ReservationState::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_active_run_round_trips() {
        let dir = tempdir("active");
        let paths = ReservationPaths::under(&dir);
        let state = ReservationState {
            active: Some(run("r1")),
            pending: None,
        };
        write_state(&paths, &state).unwrap();
        assert_eq!(read_state(&paths).unwrap(), state);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writing_none_removes_the_file() {
        let dir = tempdir("remove");
        let paths = ReservationPaths::under(&dir);
        write_state(
            &paths,
            &ReservationState {
                active: Some(run("r1")),
                pending: None,
            },
        )
        .unwrap();
        write_state(&paths, &ReservationState::default()).unwrap();
        assert!(!paths.active.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn removing_an_absent_file_is_not_an_error() {
        let dir = tempdir("absent");
        let paths = ReservationPaths::under(&dir);
        write_state(&paths, &ReservationState::default()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_run_in_both_files_is_dropped_from_pending() {
        let dir = tempdir("dup");
        let paths = ReservationPaths::under(&dir);
        write_state(
            &paths,
            &ReservationState {
                active: Some(run("same")),
                pending: Some(run("same")),
            },
        )
        .unwrap();
        let state = read_state(&paths).unwrap();
        assert_eq!(state.active.as_ref().unwrap().run_id, "same");
        assert!(state.pending.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn distinct_runs_are_both_kept() {
        let dir = tempdir("distinct");
        let paths = ReservationPaths::under(&dir);
        let state = ReservationState {
            active: Some(run("a")),
            pending: Some(run("b")),
        };
        write_state(&paths, &state).unwrap();
        assert_eq!(read_state(&paths).unwrap(), state);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_file_is_reported_not_treated_as_absent() {
        let dir = tempdir("corrupt");
        let paths = ReservationPaths::under(&dir);
        std::fs::write(&paths.active, "{not json").unwrap();
        let error = read_state(&paths).unwrap_err();
        assert!(matches!(error, ReservationStateError::Corrupt { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_write_leaves_no_temp_file_behind() {
        let dir = tempdir("notemp");
        let paths = ReservationPaths::under(&dir);
        write_state(
            &paths,
            &ReservationState {
                active: Some(run("r1")),
                pending: None,
            },
        )
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_dream_state_is_written_under_runtime_dream() {
        let dir = tempdir("dream");
        write_dream_state(&dir, "2026-11-11T00:00:00.000Z", "r9").unwrap();
        let raw = std::fs::read_to_string(dir.join("dream").join("state.json")).unwrap();
        assert!(raw.contains("last_dream_at"));
        assert!(raw.contains("r9"));
        std::fs::remove_dir_all(&dir).ok();
    }
    fn request(ids: &[&str]) -> ReflectionRequest {
        ReflectionRequest {
            trigger: ReflectionTrigger::Manual,
            origin: None,
            conversation_ids: ids.iter().map(|id| id.to_string()).collect(),
            snapshots: Vec::new(),
            focus: None,
            recent_n: None,
            target_doc: None,
        }
    }

    fn store(dir: &Path) -> ReservationStore {
        ReservationStore::new(ReservationPaths::under(dir), TriggerConfig::default())
    }

    #[test]
    fn the_first_reservation_takes_the_active_slot() {
        let dir = tempdir("store-active");
        let result = store(&dir).reserve(request(&["c1"])).unwrap();
        assert_eq!(result.status, "active");
        assert_eq!(result.run.run_id.len(), 36);
        assert!(result.run.reserved_at.is_some());
        assert!(result.run.launcher_pid.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_reservation_waits_as_pending() {
        let dir = tempdir("store-pending");
        let store = store(&dir);
        store.reserve(request(&["c1"])).unwrap();
        let second = store.reserve(request(&["c2"])).unwrap();
        assert_eq!(second.status, "pending");
        let state = store.read_state().unwrap();
        assert_eq!(state.active.unwrap().run_id.len(), 36);
        assert!(state.pending.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fixed_run_id_is_recorded_as_given() {
        let dir = tempdir("store-id");
        let store = store(&dir).with_run_id(|| "fixed-id".to_string());
        let result = store.reserve(request(&["c1"])).unwrap();
        assert_eq!(result.run.run_id, "fixed-id");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_launcher_identity_is_stamped_on_the_active_run_only() {
        let dir = tempdir("store-stamp");
        let store = store(&dir)
            .with_now_iso(|| "2026-11-11T00:00:00.000Z".to_string())
            .with_launcher_identity(|| ReflectionLauncherIdentity {
                pid: 4242,
                hostname: "unit-host".to_string(),
                process_start: Some("proc-start:7".to_string()),
            });
        let first = store.reserve(request(&["c1"])).unwrap();
        assert_eq!(first.run.launcher_pid, Some(4242));
        assert_eq!(first.run.launcher_hostname.as_deref(), Some("unit-host"));
        assert_eq!(
            first.run.reserved_at.as_deref(),
            Some("2026-11-11T00:00:00.000Z")
        );

        let second = store.reserve(request(&["c2"])).unwrap();
        assert_eq!(second.status, "pending");
        assert!(second.run.launcher_pid.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completing_an_unknown_run_does_not_corrupt_the_store() {
        let dir = tempdir("store-unknown");
        let store = store(&dir);
        store.reserve(request(&["c1"])).unwrap();
        let journals = BTreeMap::new();
        let result = store.complete("no-such-run", ReflectionOutcome::Merged, &journals);
        // Either it refuses or it leaves the active run alone; it must never write a broken state.
        let state = store.read_state().unwrap();
        assert!(state.active.is_some());
        assert!(result.is_err() || state.active.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
