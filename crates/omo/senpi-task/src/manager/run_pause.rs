//! `manager/run-pause.ts`: the per-task half of a run's pause custody.
//!
//! A paused child is held by its own engine under exactly one run owner (the parent session, the
//! parent's pause sequence, and the run). This module owns the task-record side of that custody:
//! recording the owner on the record so a restarted host re-holds the child under the same run,
//! refusing a resume whose admission was superseded, and exposing the ports a child's engine
//! implements (`ChildPausePort`).
//!
//! The controller is deliberately synchronous: every port it drives is a request/response call into
//! the child's engine, and the Rust task crate has no async store. Callers that run inside an async
//! task use `spawn_blocking`, exactly as the workflow tool does for the scheduler.

pub type WaitingState = Mutex<BTreeMap<String, (i64, Vec<tokio::sync::oneshot::Sender<()>>)>>;
pub type SettlementsState = Mutex<BTreeMap<(String, i64), Vec<tokio::sync::oneshot::Sender<()>>>>;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::manager::child_handle::ManagedChildHandle;
use crate::pause::admission_barrier::{AdmissionBarrier, AdmissionEntry, ResumeCredential};
use crate::pause::ports::RunPauseOwner;
use crate::state::{RecordedRunPause, RunPauseKind, TaskRecord, TaskStatus};
use crate::store::{StoreError, TaskRecordStore};

/// What a child's engine answers when asked to hold its next run under `owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineInheritReceipt {
    pub child_seq: u64,
}

pub use crate::pause::ports::{EngineHoldInputResult, ObservedHold};

/// A resumed child's turn: the id the engine opened, which settles with that turn's outcome.
pub struct TaskResumedTurn {
    pub turn_id: String,
    pub outcome: crate::manager::child_handle::ExactTurnOutcome,
}

/// The input a paused child holds for the resumed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldTaskInput {
    pub input_id: String,
    pub revision: u64,
    pub text: String,
}

pub enum ResumeTurnResult {
    Resumed {
        turn_id: String,
        outcome: crate::manager::child_handle::ExactTurnOutcome,
    },
    OtherHoldLive {
        child_seq: Option<u64>,
    },
}

/// `RunPauseAdmissionHeld`: the run's Resume was superseded by a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the run Resume was superseded by a hold")]
pub struct RunPauseAdmissionHeld;

/// The child engine's own pause controls (senpi#3811). A child on an older engine exposes none.
pub trait TaskChildPausePort: Send + Sync {
    /// `None` is the engine's `refused: "wrong_parent"` answer.
    fn pause_for_run(
        &self,
        child: &Arc<dyn ManagedChildHandle>,
        owner: &RunPauseOwner,
    ) -> Result<Option<EngineInheritReceipt>, String>;
    /// `None` when the child's engine reports no pause view.
    fn observe_hold(
        &self,
        _child: &Arc<dyn ManagedChildHandle>,
        _parent_session_id: &str,
    ) -> Option<ObservedHold> {
        None
    }
    fn resume_turn(
        &self,
        child: &Arc<dyn ManagedChildHandle>,
        request: &ResumeTurnRequest,
    ) -> Result<ResumeTurnResult, String>;
    fn hold_input(
        &self,
        child: &Arc<dyn ManagedChildHandle>,
        input: &HeldTaskInput,
        owner: &RunPauseOwner,
    ) -> Result<EngineHoldInputResult, String>;
}

/// A resume request, from the parent's Resume down to the child's engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTurnRequest {
    pub owner: RunPauseOwner,
    pub resume_id: String,
    pub child_seq_cutoff: u64,
}

/// A paused child that is being resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTaskRequest {
    pub allocation: Option<super::concurrency::QueuedTaskAdmission>,
    pub owner: RunPauseOwner,
    pub resume_credential: Option<ResumeCredential>,
    pub resume_id: String,
    pub child_seq_cutoff: u64,
}

/// A child the run recorded as lost: it is resumed only if its engine still holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostResumeRequest {
    pub owner: RunPauseOwner,
    pub resume_id: String,
    pub resume_credential: Option<ResumeCredential>,
    pub child_seq_cutoff: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeTaskResult {
    Resumed { turn_id: String },
    OtherHoldLive { child_seq: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostResumeResult {
    Resumed { turn_id: String },
    OtherHoldLive { child_seq: Option<u64> },
    NotHeld,
}

/// The effects a task run pause needs from the composition.
pub trait TaskRunPausePorts: Send + Sync {
    fn acquire_resume_capacity(
        &self,
        _task_id: &str,
        _epoch: i64,
        _allocation: Option<&super::concurrency::QueuedTaskAdmission>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn park_capacity(&self, _task_id: &str, _epoch: i64) {}
    fn begin_native_pause(&self, _task_id: &str, _epoch: i64) {}
    fn finish_native_pause(
        &self,
        _task_id: &str,
        _epoch: i64,
        _accepted: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn store(&self) -> &TaskRecordStore;
    /// Milliseconds since the epoch, as the store's timestamps are spelled.
    fn now(&self) -> i64;
    fn live_handle(&self, task_id: &str) -> Option<Arc<dyn ManagedChildHandle>>;
    fn child(&self) -> Option<Arc<dyn TaskChildPausePort>>;
    fn barrier(&self) -> Option<AdmissionBarrier>;
    /// Record the run's owner on the task record so a restarted host re-holds it under the same run.
    fn remember_owner(&self, task_id: &str, owner: &RunPauseOwner) -> Result<(), StoreError>;
    fn forget_owner(&self, task_id: &str) -> Result<(), StoreError>;
    /// `transition({ type: "pause" })`; `false` when the record changed under the caller.
    fn apply_pause(&self, task_id: &str, timestamp: &str) -> bool;
    /// `transition({ type: "unpause" })`; `false` when the record changed under the caller.
    fn apply_unpause(&self, task_id: &str, timestamp: &str) -> bool;
    /// Rebuild a paused child whose host is gone; it starts no turn.
    fn restore(&self, record: &TaskRecord) -> Option<Result<Arc<dyn ManagedChildHandle>, String>>;
    fn track(
        &self,
        task_id: &str,
        handle: Arc<dyn ManagedChildHandle>,
        model: &str,
        epoch: i64,
        turn: TaskResumedTurn,
    );
    /// A revive transition for an idle resident child joining a pause.
    fn revive(&self, record: &TaskRecord, timestamp: String) -> TaskRecord;
}

fn now_iso(now: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The pause intents a child's own abort can settle immediately, so registration precedes the call.
#[derive(Default)]
struct PendingPauses {
    waiting: WaitingState,
    settlements: SettlementsState,
}

impl PendingPauses {
    fn begin(&self, task_id: &str, epoch: i64) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        lock(&self.waiting)
            .entry(task_id.to_string())
            .or_insert_with(|| (epoch, Vec::new()))
            .1
            .push(sender);
        receiver
    }

    fn acknowledge(&self, task_id: &str, epoch: i64) {
        if let Some((held_epoch, waiters)) = lock(&self.waiting).remove(task_id)
            && held_epoch == epoch {
                for waiter in waiters {
                    let _ = waiter.send(());
                }
            }
    }

    fn clear(&self, task_id: &str) {
        lock(&self.waiting).remove(task_id);
        lock(&self.settlements).retain(|(id, _), _| id != task_id);
    }

    fn settle(&self, task_id: &str, epoch: i64) {
        for waiter in lock(&self.settlements)
            .remove(&(task_id.to_string(), epoch))
            .unwrap_or_default()
        {
            let _ = waiter.send(());
        }
    }

    fn settlement(&self, task_id: &str, epoch: i64) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        lock(&self.settlements)
            .entry((task_id.to_string(), epoch))
            .or_default()
            .push(sender);
        receiver
    }
}

/// A parked child gives its concurrency slot back until its Resume re-acquires one.
#[derive(Default)]
struct ResumeCapacity {
    parked: Mutex<BTreeMap<String, i64>>,
}

impl ResumeCapacity {
    fn park(&self, task_id: &str, epoch: i64) {
        lock(&self.parked).insert(task_id.to_string(), epoch);
    }

    /// A parked child needs no slot: its Resume re-acquires one before it starts a turn.
    fn acquire(&self, task_id: &str, epoch: i64) -> bool {
        let mut parked = lock(&self.parked);
        if parked.get(task_id) == Some(&epoch) {
            parked.remove(task_id);
        }
        true
    }

    fn forget(&self, task_id: &str) {
        lock(&self.parked).remove(task_id);
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The run-pause surface the task manager and the engine's pause controller drive.
pub struct TaskRunPause {
    ports: Arc<dyn TaskRunPausePorts>,
    intents: PendingPauses,
    capacity: ResumeCapacity,
    resumed: Mutex<BTreeMap<String, (String, ResumeTaskResult)>>,
    attempts: Mutex<BTreeMap<String, String>>,
    owners: Mutex<BTreeMap<String, RunPauseOwner>>,
}

impl TaskRunPause {
    pub fn new(ports: Arc<dyn TaskRunPausePorts>) -> Self {
        Self {
            ports,
            intents: PendingPauses::default(),
            capacity: ResumeCapacity::default(),
            resumed: Mutex::new(BTreeMap::new()),
            attempts: Mutex::new(BTreeMap::new()),
            owners: Mutex::new(BTreeMap::new()),
        }
    }

    fn child_port(&self) -> Result<Arc<dyn TaskChildPausePort>, String> {
        self.ports
            .child()
            .ok_or_else(|| "child pause port is not installed".to_string())
    }

    fn load(&self, task_id: &str) -> Result<TaskRecord, String> {
        match self.ports.store().load(task_id) {
            Ok(Some(record)) => Ok(record),
            Ok(None) => Err(format!("paused child {task_id} has no record")),
            Err(error) => Err(error.to_string()),
        }
    }

    fn remember(&self, task_id: &str, owner: &RunPauseOwner) -> Result<(), String> {
        lock(&self.owners).insert(task_id.to_string(), owner.clone());
        self.ports
            .remember_owner(task_id, owner)
            .map_err(|error| error.to_string())
    }

    /// `pauseTask`: hold the child under `owner`, or record the pause for a child whose host is gone.
    pub fn pause_task(
        &self,
        task_id: &str,
        owner: &RunPauseOwner,
    ) -> Result<Option<EngineInheritReceipt>, String> {
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        let handle = self.ports.live_handle(task_id);
        let epoch = record.notification.run_epoch;
        if !matches!(record.status, TaskStatus::Running | TaskStatus::Paused) {
            return Err(format!("cannot pause {} task", record.status.as_str()));
        }
        let Some(handle) = handle else {
            // Its host is gone, so nothing runs: record the pause and the run's sequence; the restore
            // that precedes Resume re-holds the child and reads the cutoff from the child's answer.
            if record.status == TaskStatus::Running
                && !self.ports.apply_pause(task_id, &now_iso(self.ports.now()))
            {
                return Err(format!("child {task_id} changed while pausing"));
            }
            self.remember(task_id, owner)?;
            return Ok(Some(EngineInheritReceipt { child_seq: 0 }));
        };
        let settled = if record.status == TaskStatus::Paused {
            None
        } else {
            Some(self.intents.begin(task_id, epoch))
        };
        lock(&self.resumed).remove(task_id);
        // Registration above MUST precede the child call: its abort can settle immediately.
        self.ports.begin_native_pause(task_id, epoch);
        let ack = match port.pause_for_run(&handle, owner) {
            Ok(ack) => ack,
            Err(error) => {
                // The request may have failed after the child's engine took the hold (an RPC
                // timeout): the engine's own hold view, not the failed call, decides.
                match port.observe_hold(&handle, &owner.parent_session_id) {
                    Some(observed) if observed.held => Some(EngineInheritReceipt {
                        child_seq: observed.child_seq,
                    }),
                    _ => {
                        if settled.is_some() {
                            self.intents.clear(task_id);
                        }
                        self.ports.finish_native_pause(task_id, epoch, false)?;
                        return Err(error);
                    }
                }
            }
        };
        let Some(ack) = ack else {
            self.ports.finish_native_pause(task_id, epoch, false)?;
            self.intents.clear(task_id);
            return Ok(None);
        };
        self.remember(task_id, owner)?;
        self.ports.finish_native_pause(task_id, epoch, true)?;
        // A pause that landed while a Resume was in flight cancels that attempt: the child stays held.
        if lock(&self.attempts).remove(task_id).is_some() && record.status == TaskStatus::Paused {
            self.park(task_id, epoch);
        }
        self.intents.acknowledge(task_id, epoch);
        Ok(Some(ack))
    }

    /// `resumePausedTask`: release the hold and open the child's next turn.
    pub fn resume_paused_task(
        &self,
        task_id: &str,
        request: &ResumeTaskRequest,
    ) -> Result<ResumeTaskResult, String> {
        if let Some((resume_id, result)) = lock(&self.resumed).get(task_id)
            && resume_id == &request.resume_id
        {
            return Ok(result.clone());
        }
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        if record.status != TaskStatus::Paused {
            return Err(format!("cannot resume {} task", record.status.as_str()));
        }
        let mut handle = self.ports.live_handle(task_id);
        let entered = self.ports.barrier().map(|barrier| {
            barrier.enter(
                &record.root_session_id.clone(),
                crate::pause::admission_barrier::AdmissionParticipant {
                    key: task_id.to_string(),
                    run_ref: request.owner.run_ref.clone(),
                },
                request.resume_credential.clone(),
            )
        });
        let ticket = match entered {
            Some(AdmissionEntry::Held { .. }) => return Err(RunPauseAdmissionHeld.to_string()),
            Some(AdmissionEntry::Entered { ticket }) => Some(ticket),
            None => None,
        };
        struct ResumeTicket(Option<crate::pause::admission_barrier::AdmissionTicket>);
        impl Drop for ResumeTicket {
            fn drop(&mut self) {
                if let Some(ticket) = &self.0 {
                    ticket.leave();
                }
            }
        }
        let _ticket_guard = ResumeTicket(ticket.clone());
        lock(&self.attempts).insert(task_id.to_string(), request.resume_id.clone());
        let mut cutoff = request.child_seq_cutoff;
        if handle.is_none() {
            let restored = self.ports.restore(&record).ok_or_else(|| {
                format!("paused child {task_id} has no live handle and no restore port")
            })??;
            let ack = port
                .pause_for_run(&restored, &request.owner)?
                .ok_or_else(|| format!("restored child {task_id} is held by another parent"))?;
            lock(&self.owners).insert(task_id.to_string(), request.owner.clone());
            cutoff = ack.child_seq;
            handle = Some(restored);
        }
        let Some(handle) = handle else {
            return Err(format!("paused child {task_id} is not attached"));
        };
        if let Some(ticket) = &ticket
            && ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed
        {
            self.park(task_id, record.notification.run_epoch);
            return Err(RunPauseAdmissionHeld.to_string());
        }
        if !self
            .capacity
            .acquire(task_id, record.notification.run_epoch)
        {
            return Err(RunPauseAdmissionHeld.to_string());
        }
        self.ports.acquire_resume_capacity(
            task_id,
            record.notification.run_epoch,
            request.allocation.as_ref(),
        )?;
        let assert_current = || -> Result<(), String> {
            let current = self.load(task_id)?;
            if lock(&self.attempts).get(task_id) != Some(&request.resume_id)
                || ticket.as_ref().is_some_and(|ticket| {
                    ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed
                })
                || current.status != TaskStatus::Paused
                || current.notification.run_epoch != record.notification.run_epoch
                || !self
                    .ports
                    .live_handle(task_id)
                    .is_some_and(|live| Arc::ptr_eq(&live, &handle))
            {
                self.park(task_id, record.notification.run_epoch);
                return Err(RunPauseAdmissionHeld.to_string());
            }
            Ok(())
        };
        assert_current()?;
        let result = port.resume_turn(
            &handle,
            &ResumeTurnRequest {
                owner: request.owner.clone(),
                resume_id: request.resume_id.clone(),
                child_seq_cutoff: cutoff,
            },
        )?;
        if let Some(ticket) = &ticket
            && ticket.check() != crate::pause::admission_barrier::AdmissionCheck::Allowed
        {
            self.park(task_id, record.notification.run_epoch);
            return Err(RunPauseAdmissionHeld.to_string());
        }
        assert_current()?;
        let outcome = match result {
            ResumeTurnResult::OtherHoldLive { child_seq } => {
                self.park(task_id, record.notification.run_epoch);
                ResumeTaskResult::OtherHoldLive { child_seq }
            }
            ResumeTurnResult::Resumed { turn_id, outcome } => {
                self.intents.clear(task_id);
                if !self
                    .ports
                    .apply_unpause(task_id, &now_iso(self.ports.now()))
                {
                    return Err("paused child changed while resuming".to_string());
                }
                lock(&self.owners).remove(task_id);
                let _ = self.ports.forget_owner(task_id);
                // The returned turn belongs to `turn_id` even when already settled; never wait here.
                self.ports.track(
                    task_id,
                    handle,
                    &record.model,
                    record.notification.run_epoch,
                    TaskResumedTurn {
                        turn_id: turn_id.clone(),
                        outcome,
                    },
                );
                ResumeTaskResult::Resumed { turn_id }
            }
        };
        lock(&self.resumed).insert(
            task_id.to_string(),
            (request.resume_id.clone(), outcome.clone()),
        );
        if let Some(ticket) = &ticket {
            ticket.leave();
        }
        Ok(outcome)
    }

    /// `observeTaskHold`: what the child's engine reports about this parent's hold.
    pub fn observe_task_hold(
        &self,
        task_id: &str,
        parent_session_id: &str,
    ) -> Option<ObservedHold> {
        let handle = self.ports.live_handle(task_id)?;
        self.ports.child()?.observe_hold(&handle, parent_session_id)
    }

    /// `resumeLostTask`: release a child the run recorded as lost but whose engine still holds it.
    pub fn resume_lost_task(
        &self,
        task_id: &str,
        request: &LostResumeRequest,
    ) -> Result<LostResumeResult, String> {
        let record = self.load(task_id)?;
        if !matches!(record.status, TaskStatus::Running | TaskStatus::Paused) {
            return Ok(LostResumeResult::NotHeld);
        }
        let cutoff = match request.child_seq_cutoff {
            Some(cutoff) => cutoff,
            None => match self.observe_task_hold(task_id, &request.owner.parent_session_id) {
                Some(observed) if observed.held => observed.child_seq,
                _ => return Ok(LostResumeResult::NotHeld),
            },
        };
        if record.status == TaskStatus::Paused {
            return Ok(
                match self.resume_paused_task(
                    task_id,
                    &ResumeTaskRequest {
                        allocation: None,
                        owner: request.owner.clone(),
                        resume_credential: request.resume_credential.clone(),
                        resume_id: request.resume_id.clone(),
                        child_seq_cutoff: cutoff,
                    },
                )? {
                    ResumeTaskResult::Resumed { turn_id } => LostResumeResult::Resumed { turn_id },
                    ResumeTaskResult::OtherHoldLive { child_seq } => {
                        LostResumeResult::OtherHoldLive { child_seq }
                    }
                },
            );
        }
        let Some(handle) = self.ports.live_handle(task_id) else {
            return Ok(LostResumeResult::NotHeld);
        };
        match self.child_port()?.resume_turn(
            &handle,
            &ResumeTurnRequest {
                owner: request.owner.clone(),
                resume_id: request.resume_id.clone(),
                child_seq_cutoff: cutoff,
            },
        )? {
            ResumeTurnResult::OtherHoldLive { child_seq } => {
                Ok(LostResumeResult::OtherHoldLive { child_seq })
            }
            ResumeTurnResult::Resumed { turn_id, outcome } => {
                self.ports.track(
                    task_id,
                    handle,
                    &record.model,
                    record.notification.run_epoch,
                    TaskResumedTurn {
                        turn_id: turn_id.clone(),
                        outcome,
                    },
                );
                Ok(LostResumeResult::Resumed { turn_id })
            }
        }
    }

    /// `holdTaskInput`: guidance names the parent that paused this child; another parent is refused.
    pub fn hold_task_input(
        &self,
        task_id: &str,
        input: &HeldTaskInput,
    ) -> Result<EngineHoldInputResult, String> {
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        if record.status != TaskStatus::Paused {
            return Err(format!(
                "cannot hold input for {} task",
                record.status.as_str()
            ));
        }
        let owner = lock(&self.owners)
            .get(task_id)
            .cloned()
            .or_else(|| saved_run_owner(&record))
            .ok_or_else(|| format!("paused child {task_id} has no recorded run owner"))?;
        let handle = match self.ports.live_handle(task_id) {
            Some(handle) => handle,
            None => self.ports.restore(&record).ok_or_else(|| {
                format!("paused child {task_id} has no live handle and no restore port")
            })??,
        };
        port.hold_input(&handle, input, &owner)
    }

    /// `joinIdlePause`: an idle resident child opens its next run paused under the run's owner.
    pub fn join_idle_pause(
        &self,
        task_id: &str,
        owner: &RunPauseOwner,
    ) -> Result<Option<EngineInheritReceipt>, String> {
        let port = self.child_port()?;
        let record = self.load(task_id)?;
        let handle = self
            .ports
            .live_handle(task_id)
            .ok_or_else(|| format!("paused child {task_id} is not attached"))?;
        if !matches!(
            record.status,
            TaskStatus::Completed | TaskStatus::Error | TaskStatus::Interrupted
        ) {
            return Err(format!(
                "cannot join an idle pause from {}",
                record.status.as_str()
            ));
        }
        let ack = port.pause_for_run(&handle, owner)?;
        if ack.is_none() {
            return Ok(None);
        }
        let timestamp = now_iso(self.ports.now());
        let mut opened = false;
        self.ports
            .store()
            .mutate(task_id, &mut |fresh: &TaskRecord| {
                if fresh.status != record.status
                    || fresh.notification.run_epoch != record.notification.run_epoch
                {
                    return fresh.clone();
                }
                opened = true;
                self.ports.revive(fresh, timestamp.clone())
            })
            .map_err(|error| error.to_string())?;
        if !opened {
            return Err(format!(
                "idle child {task_id} changed while joining the pause"
            ));
        }
        if !self.ports.apply_pause(task_id, &timestamp) {
            return Err(format!(
                "idle child {task_id} changed while joining the pause"
            ));
        }
        self.remember(task_id, owner)?;
        Ok(ack)
    }

    /// `park`: a child that stays held gives its slot back and settles its pause intent.
    pub fn park(&self, task_id: &str, epoch: i64) {
        self.capacity.park(task_id, epoch);
        self.ports.park_capacity(task_id, epoch);
        self.intents.settle(task_id, epoch);
    }

    pub fn forget(&self, task_id: &str) {
        lock(&self.owners).remove(task_id);
        self.intents.clear(task_id);
        lock(&self.attempts).remove(task_id);
        self.capacity.forget(task_id);
        lock(&self.resumed).remove(task_id);
    }

    pub fn settlement(&self, task_id: &str, epoch: i64) -> tokio::sync::oneshot::Receiver<()> {
        self.intents.settlement(task_id, epoch)
    }
}

/// The owner a restarted host saved on the record (`savedRunOwner`).
pub fn saved_run_owner(record: &TaskRecord) -> Option<RunPauseOwner> {
    let RecordedRunPause {
        parent_session_id,
        parent_pause_seq,
        run_ref,
    } = record.run_pause.clone()?;
    Some(RunPauseOwner {
        parent_session_id,
        parent_pause_seq,
        run_ref: crate::pause::ports::RunRef::new(
            match run_ref.kind {
                RunPauseKind::Dag => crate::pause::ports::RunRefKind::Dag,
                RunPauseKind::Task => crate::pause::ports::RunRefKind::Task,
                RunPauseKind::Workpool => crate::pause::ports::RunRefKind::Workpool,
                RunPauseKind::Team => crate::pause::ports::RunRefKind::Team,
            },
            run_ref.id,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumed_turn_preserves_the_exact_deferred_outcome() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let returned = ResumeTurnResult::Resumed {
            turn_id: "turn-2".into(),
            outcome: Box::new(move || {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::runners::RunnerOutcome::completed("exact turn")
            }),
        };
        let ResumeTurnResult::Resumed { turn_id, outcome } = returned else {
            panic!("resumed turn")
        };
        let tracked = TaskResumedTurn { turn_id, outcome };
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(tracked.turn_id, "turn-2");
        assert_eq!(
            (tracked.outcome)(),
            crate::runners::RunnerOutcome::completed("exact turn")
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_parked_child_keeps_no_slot_after_its_resume() {
        let capacity = ResumeCapacity::default();
        capacity.park("t1", 3);
        assert!(capacity.acquire("t1", 3));
        // A second acquire is idempotent: nothing is parked any more.
        assert!(capacity.acquire("t1", 3));
    }

    #[test]
    fn a_pause_intent_settles_when_the_child_acknowledges() {
        let intents = PendingPauses::default();
        let mut receiver = intents.begin("t1", 7);
        intents.acknowledge("t1", 7);
        assert!(receiver.try_recv().is_ok());
    }

    #[test]
    fn a_park_settles_the_pause_intent() {
        let intents = PendingPauses::default();
        let mut receiver = intents.settlement("t1", 2);
        intents.settle("t1", 2);
        assert!(receiver.try_recv().is_ok());
    }
}
