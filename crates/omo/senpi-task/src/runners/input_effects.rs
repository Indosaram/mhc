//! `runners/input-effects.ts`: durable input identity for both RPC runners.
//!
//! Every input carries a `clientMessageId`. An unacknowledged write is remembered on the record
//! (`uncertain_inputs`) and resolved once in the background: the child's admission read settles it,
//! or the SAME id and payload are sent again. A retry never changes the semantic input or its id.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::manager::start_failure_cause::RPC_RESPONSE_TIMEOUT_PREFIX;
use crate::store::{PersistedTaskEvent, TaskRecordStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InputCommandType {
    #[serde(rename = "prompt")]
    Prompt,
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "follow_up")]
    FollowUp,
}

impl InputCommandType {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputCommand {
    pub kind: InputCommandType,
    pub message: String,
    /// `"steer"` or `"followUp"`.
    pub streaming_behavior: Option<String>,
    /// The caller's own durable id for this input (a queued steer's entry id); omitted, one is minted.
    pub client_message_id: Option<String>,
}

/// The wire input; an uncertain one is stored with the run it was sent in (`run_epoch`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentifiedInput {
    #[serde(rename = "type")]
    pub kind: InputCommandType,
    pub message: String,
    #[serde(rename = "streamingBehavior", skip_serializing_if = "Option::is_none")]
    pub streaming_behavior: Option<String>,
    #[serde(rename = "clientMessageId")]
    pub client_message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_epoch: Option<i64>,
}

impl IdentifiedInput {
    /// `wireOf`: the input as the child receives it; the run it was sent in stays in omo's record.
    pub fn wire(&self) -> Value {
        let mut wire = Map::new();
        wire.insert("type".into(), self.kind.wire().into());
        wire.insert("message".into(), self.message.clone().into());
        if let Some(behavior) = &self.streaming_behavior {
            wire.insert("streamingBehavior".into(), behavior.clone().into());
        }
        wire.insert(
            "clientMessageId".into(),
            self.client_message_id.clone().into(),
        );
        Value::Object(wire)
    }
}

/// What the child engine holds for one id (`get_pending_inputs` outcomes, senpi#3549).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAdmission {
    Admitted,
    NotAdmitted,
}

pub type InputAdmissions = HashMap<String, InputAdmission>;
/// Reads each id's admission from the child; `Ok(None)` where the child cannot answer that read.
pub type InputAdmissionRead =
    Arc<dyn Fn(&[String]) -> Result<Option<InputAdmissions>, String> + Send + Sync>;

/// How a single write failed, as the input boundary needs to tell it apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputWriteError {
    /// `RpcRequestTimeout`: the request deadline passed with no acknowledgement.
    Timeout { deadline_ms: Option<u64> },
    /// `RpcTransportGoneError`: the connection dropped before the acknowledgement.
    TransportGone,
    /// The child answered and refused the input.
    Refused {
        code: Option<String>,
        message: String,
    },
}

impl InputWriteError {
    /// Classifies a refusal message the way `isUnacknowledged`/`isConflict` read an `Error`.
    pub fn from_message(code: Option<String>, message: String) -> Self {
        if message.starts_with(RPC_RESPONSE_TIMEOUT_PREFIX) {
            return Self::Timeout { deadline_ms: None };
        }
        Self::Refused { code, message }
    }

    fn is_conflict(&self) -> bool {
        matches!(self, Self::Refused { code, message }
            if code.as_deref() == Some("client_message_id_conflict")
                || message.contains("client_message_id_conflict"))
    }

    fn is_unacknowledged(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::TransportGone)
    }

    fn unacknowledged_reason(&self) -> String {
        match self {
            Self::Timeout {
                deadline_ms: Some(deadline),
            } => format!(
                "the host did not acknowledge this message within {} s",
                (*deadline as f64 / 1000.0).round()
            ),
            Self::TransportGone => {
                "the connection to the host was lost before it acknowledged this message"
                    .to_string()
            }
            _ => {
                "the host did not acknowledge this message before its request deadline".to_string()
            }
        }
    }
}

impl std::fmt::Display for InputWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { message, .. } => f.write_str(message),
            other => f.write_str(&other.unacknowledged_reason()),
        }
    }
}

pub const EFFECT_ID_CONFLICT_REASON: &str = "effect_id_conflict";
pub const EFFECT_ID_CONFLICT_DETAIL: &str =
    "this message id is already bound to a different message on the host";
pub const EFFECT_ID_CONFLICT_ACTION: &str = "send the message again";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputEffectError {
    /// `InputUncertainError`: the input may still be delivered; omo confirms it by itself.
    #[error(
        "uncertain: {reason}; it may still be delivered. omo will confirm by itself; do not resend the same text."
    )]
    Uncertain { effect_id: String, reason: String },
    /// `InputConflictError`.
    #[error(
        "this message id is already bound to a different message on the host; send the message again"
    )]
    Conflict { effect_id: String },
    #[error("{0}")]
    Failed(String),
}

impl InputEffectError {
    pub fn is_uncertain(&self) -> bool {
        matches!(self, Self::Uncertain { .. })
    }
}

pub type InputSend = Arc<dyn Fn(&IdentifiedInput) -> Result<(), InputWriteError> + Send + Sync>;

pub struct InputEffectsOptions {
    pub task_id: String,
    pub store: Option<TaskRecordStore>,
    pub send: InputSend,
    /// The child's admission read (#2838 step 4). With it an unacknowledged input is resolved by its
    /// id before anything is resent: admitted settles it, not_admitted resends the SAME id, an
    /// unreadable answer keeps it uncertain. Without it the same-id resend runs as before.
    pub read: Option<InputAdmissionRead>,
}

#[derive(Default)]
struct Latch {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Latch {
    fn release(&self) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        while !*done {
            done = self
                .changed
                .wait(done)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

enum AdmissionAnswer {
    NoRead,
    Unreadable,
    Read(InputAdmissions),
}

struct Inner {
    options: InputEffectsOptions,
    local_seq: AtomicU64,
    pending: Mutex<Vec<IdentifiedInput>>,
    retries: Mutex<HashMap<String, Arc<Latch>>>,
    scheduled: Mutex<Vec<std::thread::JoinHandle<()>>>,
    retrying: AtomicBool,
}

/// `createInputEffects`.
#[derive(Clone)]
pub struct InputEffects(Arc<Inner>);

pub fn create_input_effects(options: InputEffectsOptions) -> InputEffects {
    InputEffects(Arc::new(Inner {
        options,
        local_seq: AtomicU64::new(0),
        pending: Mutex::new(Vec::new()),
        retries: Mutex::new(HashMap::new()),
        scheduled: Mutex::new(Vec::new()),
        retrying: AtomicBool::new(true),
    }))
}

impl Inner {
    fn task_id(&self) -> &str {
        &self.options.task_id
    }

    fn mutate(&self, mutation: impl FnOnce(&crate::state::TaskRecord) -> crate::state::TaskRecord) {
        if let Some(store) = &self.options.store
            && let Err(error) = store.mutate(self.task_id(), mutation)
        {
            log_failure(
                "senpi-task input record update failed",
                self.task_id(),
                &error.to_string(),
            );
        }
    }

    fn remember(&self, command: &IdentifiedInput) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            pending.retain(|item| item.client_message_id != command.client_message_id);
            pending.push(command.clone());
        }
        self.mutate(|record| {
            let mut next = record.clone();
            let mut inputs: Vec<IdentifiedInput> = record
                .uncertain_inputs
                .iter()
                .flatten()
                .filter(|item| item.client_message_id != command.client_message_id)
                .cloned()
                .collect();
            inputs.push(command.clone());
            next.uncertain_inputs = Some(inputs);
            next
        });
    }

    fn forget(&self, command: &IdentifiedInput) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|item| item.client_message_id != command.client_message_id);
        self.mutate(|record| {
            let mut next = record.clone();
            let remaining: Vec<IdentifiedInput> = record
                .uncertain_inputs
                .iter()
                .flatten()
                .filter(|item| item.client_message_id != command.client_message_id)
                .cloned()
                .collect();
            next.uncertain_inputs = (!remaining.is_empty()).then_some(remaining);
            next
        });
    }

    fn event(&self, event_type: &str, command: &IdentifiedInput, extra: Value) {
        let Some(store) = &self.options.store else {
            return;
        };
        let mut payload = json!({
            "effect_id": command.client_message_id,
            "run_epoch": epoch_of(command),
        });
        if let (Some(payload), Value::Object(extra)) = (payload.as_object_mut(), extra) {
            payload.extend(extra);
        }
        let event = PersistedTaskEvent {
            event_type: event_type.to_string(),
            payload,
        };
        if let Err(error) = store.append_event(self.task_id(), &event) {
            log_failure(
                "senpi-task input event append failed",
                self.task_id(),
                &error.to_string(),
            );
        }
    }

    fn settled(&self, command: &IdentifiedInput) {
        self.forget(command);
        let started =
            command.kind == InputCommandType::Prompt && command.streaming_behavior.is_none();
        let steered = command.kind == InputCommandType::Steer
            || command.streaming_behavior.as_deref() == Some("steer");
        self.event(
            if started { "started" } else { "steered" },
            command,
            json!({ "delivered": if steered { "steer" } else { "followUp" } }),
        );
    }

    fn admissions(&self, ids: &[String]) -> AdmissionAnswer {
        let Some(read) = &self.options.read else {
            return AdmissionAnswer::NoRead;
        };
        if ids.is_empty() {
            return AdmissionAnswer::NoRead;
        }
        match read(ids) {
            Ok(Some(admissions)) => AdmissionAnswer::Read(admissions),
            Ok(None) => AdmissionAnswer::NoRead,
            Err(error) => {
                log_failure(
                    "senpi-task input admission read failed",
                    self.task_id(),
                    &error,
                );
                AdmissionAnswer::Unreadable
            }
        }
    }

    fn retrying(&self) -> bool {
        self.retrying.load(Ordering::Acquire)
    }

    /// Joins an in-flight retry of the same id, or runs one on this thread.
    fn retry(&self, command: &IdentifiedInput) {
        if !self.retrying() {
            return;
        }
        let (latch, owner) = {
            let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
            match retries.get(&command.client_message_id) {
                Some(active) => (Arc::clone(active), false),
                None => {
                    let latch = Arc::new(Latch::default());
                    retries.insert(command.client_message_id.clone(), Arc::clone(&latch));
                    (latch, true)
                }
            }
        };
        if !owner {
            latch.wait();
            return;
        }
        self.retry_once(command);
        self.retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&command.client_message_id);
        latch.release();
    }

    fn retry_once(&self, command: &IdentifiedInput) {
        match self.admissions(std::slice::from_ref(&command.client_message_id)) {
            AdmissionAnswer::Unreadable => return,
            _ if !self.retrying() => return,
            AdmissionAnswer::Read(read)
                if read.get(&command.client_message_id) == Some(&InputAdmission::Admitted) =>
            {
                self.settled(command);
                return;
            }
            _ => {}
        }
        match (self.options.send)(command) {
            Ok(()) => {
                if self.retrying() {
                    self.settled(command);
                }
            }
            Err(_) if !self.retrying() => {}
            Err(error) if error.is_unacknowledged() => {}
            Err(error) => {
                self.forget(command);
                let extra = if error.is_conflict() {
                    json!({
                        "reason": EFFECT_ID_CONFLICT_REASON,
                        "detail": EFFECT_ID_CONFLICT_DETAIL,
                        "action": EFFECT_ID_CONFLICT_ACTION,
                    })
                } else {
                    json!({
                        "reason": "input_refused",
                        "detail": error.to_string(),
                        "action": "inspect task_output before sending another message",
                    })
                };
                self.event("effect_failed", command, extra);
            }
        }
    }
}

impl InputEffects {
    pub fn send(&self, command: InputCommand) -> Result<(), InputEffectError> {
        let send = Arc::clone(&self.0.options.send);
        self.send_with(command, move |identified| send(identified))
    }

    /// `send(command, write)`: the same identity boundary over a caller-chosen write.
    pub fn send_with(
        &self,
        command: InputCommand,
        write: impl Fn(&IdentifiedInput) -> Result<(), InputWriteError>,
    ) -> Result<(), InputEffectError> {
        let inner = &self.0;
        let own = command.client_message_id.clone();
        let mut id = own.clone().unwrap_or_else(|| {
            format!(
                "{}:0:{}",
                inner.task_id(),
                inner.local_seq.fetch_add(1, Ordering::AcqRel) + 1
            )
        });
        let mut run_epoch = None;
        if let Some(store) = &inner.options.store {
            let mutated = store.mutate(inner.task_id(), |record| {
                let mut next = record.clone();
                // A caller's own id (a queued steer's entry id) is sent as it is.
                if let Some(own) = &own {
                    next.last_effect_id = Some(own.clone());
                    return next;
                }
                let seq = record.effect_seq.unwrap_or(0).max(
                    record
                        .host_session
                        .as_ref()
                        .and_then(|s| s.effect_seq)
                        .unwrap_or(0),
                ) + 1;
                let minted = format!(
                    "{}:{}:{}",
                    inner.task_id(),
                    record.notification.run_epoch,
                    seq
                );
                next.effect_seq = Some(seq);
                next.last_effect_id = Some(minted);
                if let Some(host) = next.host_session.as_mut() {
                    host.effect_seq = Some(seq);
                }
                next
            });
            match mutated {
                Ok(Some(record)) => {
                    run_epoch = Some(record.notification.run_epoch);
                    if own.is_none()
                        && let Some(minted) = record.last_effect_id
                    {
                        id = minted;
                    }
                }
                Ok(None) => {}
                Err(error) => return Err(InputEffectError::Failed(error.to_string())),
            }
        }
        let identified = IdentifiedInput {
            kind: command.kind,
            message: command.message,
            streaming_behavior: command.streaming_behavior,
            client_message_id: id.clone(),
            run_epoch: None,
        };
        let error = match write(&identified) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if error.is_conflict() {
            return Err(InputEffectError::Conflict { effect_id: id });
        }
        if !error.is_unacknowledged() {
            return Err(InputEffectError::Failed(error.to_string()));
        }
        let remembered = IdentifiedInput {
            run_epoch,
            ..identified.clone()
        };
        inner.remember(&remembered);
        inner.event("effect_uncertain", &identified, json!({}));
        if inner.retrying() {
            // Yield to the caller's uncertain result before the one automatic retry is issued.
            let background = Arc::clone(inner);
            let spawned = std::thread::Builder::new()
                .name("senpi-task-input-retry".into())
                .spawn(move || background.retry(&remembered));
            match spawned {
                Ok(handle) => inner
                    .scheduled
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(handle),
                Err(error) => log_failure(
                    "senpi-task input resolution failed",
                    inner.task_id(),
                    &error.to_string(),
                ),
            }
        }
        Err(InputEffectError::Uncertain {
            effect_id: id,
            reason: error.unacknowledged_reason(),
        })
    }

    pub fn reconcile(&self) {
        let inner = &self.0;
        let record = inner
            .options
            .store
            .as_ref()
            .and_then(|store| store.load(inner.task_id()).ok().flatten());
        let commands = match &record {
            Some(record) => record.uncertain_inputs.clone().unwrap_or_default(),
            None => inner
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        };
        let current_epoch = record.as_ref().map(|record| record.notification.run_epoch);
        // An id whose retry is already in flight is settled by that retry, so the log never records
        // it both ways.
        let in_flight: HashSet<String> = inner
            .retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        let (stale, live): (Vec<_>, Vec<_>) = commands.into_iter().partition(|command| {
            current_epoch.is_some_and(|epoch| epoch_of(command) != Some(epoch))
                && !in_flight.contains(&command.client_message_id)
        });
        // An earlier run's input is never sent into a later run. Where the child can answer, its read
        // decides: admitted is settled, not_admitted fails as superseded, unreadable stays uncertain.
        let ids: Vec<String> = stale
            .iter()
            .map(|command| command.client_message_id.clone())
            .collect();
        let read = inner.admissions(&ids);
        for command in &stale {
            match &read {
                AdmissionAnswer::Unreadable => continue,
                AdmissionAnswer::Read(read)
                    if read.get(&command.client_message_id) == Some(&InputAdmission::Admitted) =>
                {
                    inner.settled(command);
                    continue;
                }
                _ => {}
            }
            inner.forget(command);
            inner.event(
                "effect_failed",
                command,
                json!({
                    "reason": "superseded_by_new_run",
                    "detail": "this message was never confirmed before its run ended, so it is not delivered into a later run",
                    "action": "send the message again if the task still needs it",
                }),
            );
        }
        let workers: Vec<_> = live
            .into_iter()
            .map(|command| {
                let inner = Arc::clone(inner);
                std::thread::spawn(move || inner.retry(&command))
            })
            .collect();
        for worker in workers {
            let _ = worker.join();
        }
    }

    pub fn stop_retrying(&self) {
        self.0.retrying.store(false, Ordering::Release);
        self.0
            .scheduled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|handle| !handle.is_finished());
    }

    /// Joins every background retry scheduled so far (tests await resolution through this).
    #[cfg(test)]
    pub(crate) fn join_scheduled(&self) {
        let handles = std::mem::take(
            &mut *self
                .0
                .scheduled
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for handle in handles {
            handle.join().expect("input retry thread");
        }
    }
}

fn epoch_of(command: &IdentifiedInput) -> Option<i64> {
    command.run_epoch.or_else(|| {
        let parts: Vec<&str> = command.client_message_id.split(':').collect();
        parts
            .len()
            .checked_sub(2)
            .and_then(|index| parts[index].parse().ok())
    })
}

fn log_failure(message: &str, task_id: &str, error: &str) {
    utils::logger::log(message, Some(&json!({ "taskId": task_id, "error": error })));
}

#[cfg(test)]
#[path = "input_effects_tests.rs"]
mod tests;
