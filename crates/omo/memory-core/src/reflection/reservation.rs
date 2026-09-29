//! File-backed reflection reservation store managing active locks and pending queues.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::identity::resolve::MemoryIdentity;
use crate::journal::store::TranscriptJournal;
use crate::locks::acquire::{AcquireLockOptions, WithLockError, with_lock};
use crate::locks::domains::reflection_scheduler_lock_path;
use crate::locks::lock_record::{CreateLockRecordOptions, create_lock_record};
use crate::locks::process_identity::get_process_start_identity;
use crate::support::host::hostname;
use crate::support::random::random_uuid;
use crate::support::time::now_iso;

use super::machine::{
    CapturedConversation, EvaluationAction, JournalSnapshot, MachineState, ReflectionEvent,
    ReflectionOutcome, ReflectionRequest, ReservationState, ReservedRun, TriggerConfig,
    complete_transition, evaluate_transitions, reserve_transition,
};

/// Resolves the transcript journal for a session id.
pub type GetJournal =
    Arc<dyn Fn(&str) -> Result<TranscriptJournal, ReservationError> + Send + Sync>;

/// Identity details of the launcher recorded on an active reflection run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionLauncherIdentity {
    pub pid: u32,
    pub hostname: String,
    pub process_start: Option<String>,
}

/// Configuration options for instantiating the reflection reservation store.
pub struct ReflectionReservationStoreOptions {
    pub identity: MemoryIdentity,
    pub config: TriggerConfig,
    pub get_journal: GetJournal,
    pub create_run_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub now_iso: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub launcher_identity: Option<Arc<dyn Fn() -> ReflectionLauncherIdentity + Send + Sync>>,
}

/// Outcome of attempting to reserve a reflection run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationResult {
    pub status: String,
    pub run: ReservedRun,
}

/// Result returned upon completing an active reflection run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResult {
    pub outcome: ReflectionOutcome,
    pub launch: Option<ReservedRun>,
}

/// Errors raised during reservation lifecycle operations.
#[derive(Debug)]
pub enum ReservationError {
    Lock(String),
    Io(std::io::Error),
    CorruptState(String),
    TransitionFailed(String),
    Journal(String),
}

impl fmt::Display for ReservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(msg) => write!(formatter, "scheduler lock error: {msg}"),
            Self::Io(err) => write!(formatter, "reservation store io error: {err}"),
            Self::CorruptState(msg) => write!(formatter, "corrupt reservation state: {msg}"),
            Self::TransitionFailed(msg) => write!(formatter, "transition failed: {msg}"),
            Self::Journal(msg) => write!(formatter, "journal error: {msg}"),
        }
    }
}

impl std::error::Error for ReservationError {}

impl From<std::io::Error> for ReservationError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// File-backed persistent reservation store coordinating exclusive active and single pending runs.
pub struct ReflectionReservationStore {
    identity: MemoryIdentity,
    config: TriggerConfig,
    active_path: PathBuf,
    pending_path: PathBuf,
    scheduler_lock_path: PathBuf,
    get_journal: GetJournal,
    create_run_id: Arc<dyn Fn() -> String + Send + Sync>,
    now_iso: Arc<dyn Fn() -> String + Send + Sync>,
    launcher_identity: Arc<dyn Fn() -> ReflectionLauncherIdentity + Send + Sync>,
}

impl ReflectionReservationStore {
    /// Constructs a new reflection reservation store for the given memory identity.
    pub fn new(options: ReflectionReservationStoreOptions) -> Self {
        let active_path = options.identity.paths.reflection.join("active.lock");
        let pending_path = options.identity.paths.reflection.join("pending.json");
        let scheduler_lock_path = reflection_scheduler_lock_path(&options.identity.paths.locks);

        Self {
            identity: options.identity,
            config: options.config,
            active_path,
            pending_path,
            scheduler_lock_path,
            get_journal: options.get_journal,
            create_run_id: options
                .create_run_id
                .unwrap_or_else(|| Arc::new(random_uuid)),
            now_iso: options.now_iso.unwrap_or_else(|| Arc::new(now_iso)),
            launcher_identity: options
                .launcher_identity
                .unwrap_or_else(|| Arc::new(default_launcher_identity)),
        }
    }

    /// Evaluates a boundary event and reserves reflection if triggered.
    pub fn evaluate(
        &self,
        conversation_id: &str,
        event: ReflectionEvent,
    ) -> Result<Option<ReservationResult>, ReservationError> {
        let journal = (self.get_journal)(conversation_id)?;
        if event == ReflectionEvent::CompactionAccepted {
            journal
                .set_pending_compaction(true)
                .map_err(|e| ReservationError::Journal(e.to_string()))?;
            return Ok(None);
        }

        let state = journal
            .get_state()
            .map_err(|e| ReservationError::Journal(e.to_string()))?;
        let reservation = self.read_state()?;
        let evaluated = evaluate_transitions(
            MachineState {
                journal: JournalSnapshot {
                    conversation_id: conversation_id.to_string(),
                    state,
                    snapshot: None,
                },
                reservation,
                config: self.config.clone(),
            },
            event,
        );

        let request = match evaluated.action {
            EvaluationAction::Reserve(r) => r,
            EvaluationAction::None => return Ok(None),
        };

        let mut snapshots = Vec::new();
        for id in &request.conversation_ids {
            let j = (self.get_journal)(id)?;
            if let Some(snap) = j
                .capture_reflection_snapshot(None)
                .map_err(|e| ReservationError::Journal(e.to_string()))?
            {
                snapshots.push(CapturedConversation {
                    conversation_id: id.clone(),
                    snapshot: snap,
                });
            }
        }

        let full_request = ReflectionRequest {
            snapshots,
            ..request
        };

        self.try_reserve(full_request).map(Some)
    }

    /// Attempts to reserve an active slot or merges into the pending queue.
    pub fn try_reserve(
        &self,
        request: ReflectionRequest,
    ) -> Result<ReservationResult, ReservationError> {
        let run_id = (self.create_run_id)();
        self.locked(Some(&run_id), || {
            let current = self.read_state_unlocked()?;
            let (mut transition_state, result_kind) =
                reserve_transition(current, request, run_id.clone());

            if result_kind == "active"
                && let Some(active) = &mut transition_state.active
            {
                self.stamp_launch_owner(active);
            }

            self.write_state_unlocked(&transition_state)?;

            let run = if result_kind == "active" {
                transition_state.active.ok_or_else(|| {
                    ReservationError::TransitionFailed("No active run".to_string())
                })?
            } else {
                transition_state.pending.ok_or_else(|| {
                    ReservationError::TransitionFailed("No pending run".to_string())
                })?
            };

            Ok(ReservationResult {
                status: result_kind.to_string(),
                run,
            })
        })
    }

    /// Completes an active reflection run, updating journals and promoting pending runs.
    pub fn complete(
        &self,
        run_id: &str,
        outcome: ReflectionOutcome,
    ) -> Result<CompletionResult, ReservationError> {
        self.locked(Some(run_id), || {
            let current = self.read_state_unlocked()?;
            let mut conversation_ids = BTreeSet::new();
            if let Some(a) = &current.active {
                conversation_ids.extend(a.request.conversation_ids.iter().cloned());
            }
            if let Some(p) = &current.pending {
                conversation_ids.extend(p.request.conversation_ids.iter().cloned());
            }

            let mut journals = BTreeMap::new();
            let mut snapshots = BTreeMap::new();
            for id in conversation_ids {
                let journal = (self.get_journal)(&id)?;
                let state = journal
                    .get_state()
                    .map_err(|e| ReservationError::Journal(e.to_string()))?;
                snapshots.insert(
                    id.clone(),
                    JournalSnapshot {
                        conversation_id: id.clone(),
                        state,
                        snapshot: None,
                    },
                );
                journals.insert(id, journal);
            }

            let transition =
                complete_transition(current, run_id, outcome, &snapshots, &self.config)
                    .map_err(ReservationError::TransitionFailed)?;

            for captured in &transition.finalize {
                if let Some(journal) = journals.get(&captured.conversation_id) {
                    journal
                        .finalize_reflection(&captured.snapshot, true)
                        .map_err(|e| ReservationError::Journal(e.to_string()))?;
                }
            }

            for id in &transition.clear_pending_compaction {
                if let Some(journal) = journals.get(id) {
                    journal
                        .set_pending_compaction(false)
                        .map_err(|e| ReservationError::Journal(e.to_string()))?;
                }
            }

            let mut next_state = transition.state;
            let mut launch = transition.launch;
            if let Some(promoted) = &mut launch {
                self.stamp_launch_owner(promoted);
                next_state.active = Some(promoted.clone());
            }

            self.write_state_unlocked(&next_state)?;

            Ok(CompletionResult { outcome, launch })
        })
    }

    /// Reads the current reservation state from disk under lock.
    pub fn read_state(&self) -> Result<ReservationState, ReservationError> {
        self.locked(None, || self.read_state_unlocked())
    }

    fn stamp_launch_owner(&self, run: &mut ReservedRun) {
        let launcher = (self.launcher_identity)();
        run.reserved_at = Some((self.now_iso)());
        run.launcher_pid = Some(launcher.pid);
        run.launcher_hostname = Some(launcher.hostname);
        run.launcher_process_start = launcher.process_start;
    }

    fn locked<T, F>(&self, run_id: Option<&str>, task: F) -> Result<T, ReservationError>
    where
        F: FnOnce() -> Result<T, ReservationError>,
    {
        let record = create_lock_record(
            "reflection-scheduler",
            CreateLockRecordOptions {
                run_id: run_id.map(String::from),
            },
        )
        .map_err(|e| ReservationError::Lock(e.to_string()))?;

        let acquire_options = AcquireLockOptions {
            wait_timeout_ms: Some(5_000),
            ..Default::default()
        };

        with_lock(&self.scheduler_lock_path, &record, &acquire_options, task).map_err(|err| {
            match err {
                WithLockError::Acquire(e) => ReservationError::Lock(e.to_string()),
                WithLockError::User(e) => e,
            }
        })
    }

    fn read_state_unlocked(&self) -> Result<ReservationState, ReservationError> {
        let active = read_run(&self.active_path)?;
        let pending = read_run(&self.pending_path)?;

        let pending = match (active.as_ref(), pending) {
            (Some(a), Some(p)) if a.run_id == p.run_id => None,
            (_, p) => p,
        };

        Ok(ReservationState { active, pending })
    }

    fn write_state_unlocked(&self, state: &ReservationState) -> Result<(), ReservationError> {
        std::fs::create_dir_all(&self.identity.paths.reflection)?;
        write_optional_run(&self.active_path, state.active.as_ref())?;
        write_optional_run(&self.pending_path, state.pending.as_ref())?;
        Ok(())
    }
}

fn read_run(path: &Path) -> Result<Option<ReservedRun>, ReservationError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ReservationError::Io(e)),
    };
    let parsed: ReservedRun = serde_json::from_str(&raw).map_err(|e| {
        ReservationError::CorruptState(format!(
            "Invalid reflection reservation {}: {e}",
            path.display()
        ))
    })?;
    Ok(Some(parsed))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_name = format!(
        "{}.tmp-{}",
        path.file_name().and_then(|f| f.to_str()).unwrap_or("run"),
        crate::support::random::random_id()
    );
    let temp_path = path.with_file_name(temp_name);
    let mut file = std::fs::File::create(&temp_path)?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    use std::io::Write;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temp_path, path)
}

fn write_optional_run(path: &Path, run: Option<&ReservedRun>) -> std::io::Result<()> {
    match run {
        Some(r) => write_json_atomic(path, r),
        None => {
            if let Err(e) = std::fs::remove_file(path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(e);
            }
            Ok(())
        }
    }
}

fn default_launcher_identity() -> ReflectionLauncherIdentity {
    let pid = std::process::id();
    ReflectionLauncherIdentity {
        pid,
        hostname: hostname(),
        process_start: get_process_start_identity(pid),
    }
}

#[cfg(test)]
#[path = "reservation_tests.rs"]
mod tests;
