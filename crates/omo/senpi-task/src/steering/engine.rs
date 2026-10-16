//! The steering engine (`steering/engine.ts`).
//!
//! Prelaunch steering is durable: sends to a still-pending child append to the record's
//! `pending_steering`, so the queue survives a restart and drains in persisted order when the
//! child launches. The record is the single source of truth - no in-memory shadow.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use super::types::{
    CancelAbort, CancelOptions, CancelOutcome, DEFAULT_SEND_DELIVERY, InterruptOutcome, SendInput,
    SendOutcome, SteeringError, SteeringPort,
};
use crate::agents::interaction_policy_for_agent;
use crate::host::HostError;
use crate::lifecycle::DestroyCause;
use crate::manager::ManagedChildHandle;
use crate::shared::iso_from_ms;
use crate::state::{
    DeliverAs, Messageability, PendingSteeringEntry, ResidencyState, TaskRecord, TaskStatus,
    TaskTransition, messageability,
};
use crate::store::PersistedTaskEvent;

const TASK_OUTPUT_SUGGESTION: &str = "Use task_output to read the final result.";
const NOT_FOUND_SUGGESTION: &str =
    "Use /tasks to see available tasks, or task_output to read a known task.";

pub struct SteeringEngine {
    port: SteeringPort,
    active_sends: Mutex<HashSet<String>>,
}

struct SendLease<'a> {
    active: &'a Mutex<HashSet<String>>,
    task_id: String,
}

impl Drop for SendLease<'_> {
    fn drop(&mut self) {
        self.active.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.task_id);
    }
}

fn deliver_as_str(deliver_as: DeliverAs) -> &'static str {
    match deliver_as {
        DeliverAs::Steer => "steer",
        DeliverAs::FollowUp => "followUp",
    }
}

fn deliver(
    handle: &dyn ManagedChildHandle,
    deliver_as: DeliverAs,
    message: &str,
) -> Result<(), HostError> {
    match deliver_as {
        DeliverAs::Steer => handle.steer(message),
        DeliverAs::FollowUp => handle.follow_up(message),
    }
}

fn not_found(id_or_name: &str) -> SendOutcome {
    SendOutcome::NotFound {
        reason: format!("No task found for \"{id_or_name}\"."),
        suggestion: NOT_FOUND_SUGGESTION.to_string(),
    }
}

fn stale_send(record: &TaskRecord) -> SendOutcome {
    SendOutcome::Stale {
        task_id: record.task_id.clone(),
        run_epoch: record.notification.run_epoch,
        reason: format!("Task {}: the handle names an earlier run of this task.", record.task_id),
    }
}

fn stale_cancel(record: &TaskRecord) -> CancelOutcome {
    CancelOutcome::Stale {
        task_id: record.task_id.clone(),
        status: record.status,
        run_epoch: record.notification.run_epoch,
        reason: format!("Task {}: the handle names an earlier run of this task.", record.task_id),
    }
}

impl SteeringEngine {
    pub fn new(port: SteeringPort) -> Self {
        Self { port, active_sends: Mutex::new(HashSet::new()) }
    }

    pub fn has_in_flight_send(&self, task_id: &str) -> bool {
        self.active_sends.lock().unwrap_or_else(PoisonError::into_inner).contains(task_id)
    }

    pub fn has_pending_sends(&self, task_id: &str) -> bool {
        self.has_in_flight_send(task_id)
            || self.try_load(task_id).is_some_and(|record| record.pending_steering.as_ref().is_some_and(|queue| !queue.is_empty()))
    }

    pub fn try_begin_send(&self, task_id: &str) -> bool {
        self.begin_send(task_id)
    }

    pub fn end_send(&self, task_id: &str) {
        self.active_sends.lock().unwrap_or_else(PoisonError::into_inner).remove(task_id);
    }

    pub fn release_send_lease(&self, task_id: &str) {
        self.end_send(task_id);
    }

    fn begin_send(&self, task_id: &str) -> bool {
        self.active_sends.lock().unwrap_or_else(PoisonError::into_inner).insert(task_id.to_string())
    }

    fn event(&self, task_id: &str, event_type: &str, payload: Value) -> Result<(), SteeringError> {
        self.port.store.append_event(
            task_id,
            &PersistedTaskEvent {
                event_type: event_type.to_string(),
                payload,
            },
        )?;
        Ok(())
    }

    fn now_iso(&self) -> String {
        iso_from_ms((self.port.now)())
    }

    fn try_load(&self, task_id: &str) -> Option<TaskRecord> {
        self.port.store.load(task_id).ok().flatten()
    }

    fn resolve(&self, id_or_name: &str) -> Result<Option<TaskRecord>, SteeringError> {
        if let Some(record) = self.try_load(id_or_name) {
            return Ok(Some(record));
        }
        Ok(self
            .port
            .store
            .list()?
            .records
            .into_iter()
            .find(|record| record.name.as_deref() == Some(id_or_name)))
    }

    pub fn send_to_task(&self, input: &SendInput) -> Result<SendOutcome, SteeringError> {
        let Some(record) = self.resolve(&input.id_or_name)? else {
            return Ok(not_found(&input.id_or_name));
        };
        if let Some(denied) = scope_denied(&record, input) {
            return Ok(denied);
        }
        if let Some(expected) = input.expected_run_epoch
            && record.notification.run_epoch != expected
        {
            return Ok(stale_send(&record));
        }
        if !self.begin_send(&record.task_id) {
            return Ok(SendOutcome::NotContinuable {
                task_id: record.task_id.clone(),
                reason: format!("Task {} already has a steering operation in progress.", record.task_id),
                suggestion: TASK_OUTPUT_SUGGESTION.to_string(),
            });
        }
        let _lease = SendLease { active: &self.active_sends, task_id: record.task_id.clone() };
        self.send_resolved(input, record)
    }

    fn send_resolved(&self, input: &SendInput, record: TaskRecord) -> Result<SendOutcome, SteeringError> {
        // One-shot policy runs after ownership is established but BEFORE the pending enqueue.
        if let Some(one_shot) = one_shot_policy_denial(&record) {
            return Ok(one_shot);
        }
        let deliver_as = input.deliver_as.unwrap_or(DEFAULT_SEND_DELIVERY);
        if record.status == TaskStatus::Pending {
            return self.enqueue_pending(&record, &input.message, deliver_as);
        }
        let mode = messageability(record.status, record.residency_state);
        if mode == Messageability::NotContinuable {
            return Ok(SendOutcome::NotContinuable {
                task_id: record.task_id.clone(),
                reason: not_continuable_reason(&record),
                suggestion: TASK_OUTPUT_SUGGESTION.to_string(),
            });
        }
        let Some(handle) = (self.port.live_handle)(&record.task_id) else {
            return Ok(SendOutcome::NotContinuable {
                reason: format!(
                    "Task {} has no resident session in this process.",
                    record.task_id
                ),
                task_id: record.task_id,
                suggestion: TASK_OUTPUT_SUGGESTION.to_string(),
            });
        };
        let Some(fresh) = self.port.store.load(&record.task_id)? else {
            return Ok(not_found(&record.task_id));
        };
        if fresh.notification.run_epoch != record.notification.run_epoch
            || fresh.status != record.status
            || fresh.residency_state != record.residency_state
        {
            return Ok(stale_send(&fresh));
        }
        if mode == Messageability::Steer {
            deliver(handle.as_ref(), deliver_as, &input.message)?;
            self.event(
                &record.task_id,
                "steered",
                json!({ "delivered": deliver_as_str(deliver_as), "run_epoch": record.notification.run_epoch }),
            )?;
            return Ok(SendOutcome::Steered {
                task_id: record.task_id,
                status: record.status,
                delivered: deliver_as,
            });
        }
        // Revive is a follow-up prompt on the SAME session, not a fresh child.
        let mut revived_record = None;
        let updated = self.port.store.mutate(&record.task_id, |fresh| {
            if fresh.notification.run_epoch != record.notification.run_epoch
                || fresh.status != record.status
                || fresh.residency_state != record.residency_state
                || !fresh.status.is_terminal()
            {
                return fresh.clone();
            }
            let next = build_revived(fresh, self.now_iso());
            revived_record = Some(next.clone());
            next
        })?;
        let Some(revived) = revived_record.filter(|_| updated.is_some()) else {
            let fresh = self.port.store.load(&record.task_id)?.ok_or_else(|| SteeringError::Store(crate::store::StoreError::NotFound(record.task_id.clone())))?;
            return Ok(stale_send(&fresh));
        };
        let run_epoch = revived.notification.run_epoch;
        self.event(
            &record.task_id,
            "revived",
            json!({ "run_epoch": run_epoch }),
        )?;
        handle.follow_up(&input.message)?;
        let Some(acknowledged) = self.port.store.load(&record.task_id)? else {
            return Ok(not_found(&record.task_id));
        };
        if acknowledged.status != TaskStatus::Running
            || acknowledged.host_pid != revived.host_pid
            || acknowledged.notification.run_epoch != run_epoch
        {
            return Ok(stale_send(&acknowledged));
        }
        (self.port.reacquire_for_revive)(&record.task_id);
        Ok(SendOutcome::Revived {
            task_id: record.task_id,
            run_epoch,
        })
    }

    fn enqueue_pending(
        &self,
        record: &TaskRecord,
        message: &str,
        deliver_as: DeliverAs,
    ) -> Result<SendOutcome, SteeringError> {
        let mut position = 0;
        let now = (self.port.now)();
        let updated = self.port.store.mutate(&record.task_id, |fresh| {
            let mut queue = fresh.pending_steering.clone().unwrap_or_default();
            queue.push(PendingSteeringEntry {
                id: format!("ps-{now}-{}", queue.len() + 1),
                message: message.to_string(),
                deliver_as,
                workpool: None,
            });
            position = queue.len();
            TaskRecord {
                pending_steering: Some(queue),
                ..fresh.clone()
            }
        })?;
        if updated.is_none() {
            return Ok(not_found(&record.task_id));
        }
        self.event(
            &record.task_id,
            "steer_queued",
            json!({ "queue_position": position, "deliverAs": deliver_as_str(deliver_as) }),
        )?;
        Ok(SendOutcome::Queued {
            task_id: record.task_id.clone(),
            queue_position: position,
        })
    }

    /// Discards a pending child's durable queue (the child will never start).
    pub fn drop_pending(&self, task_id: &str) -> Result<(), SteeringError> {
        self.clear_persisted_queue(task_id, None)
    }

    /// With `drained_ids`, only the just-delivered entries are cleared so a concurrent enqueue
    /// survives; without it the whole queue goes.
    fn clear_persisted_queue(
        &self,
        task_id: &str,
        drained_ids: Option<&BTreeSet<String>>,
    ) -> Result<(), SteeringError> {
        self.port.store.mutate(task_id, |fresh| {
            let Some(queue) = fresh.pending_steering.as_ref().filter(|q| !q.is_empty()) else {
                return fresh.clone();
            };
            let remaining: Vec<PendingSteeringEntry> = match drained_ids {
                None => Vec::new(),
                Some(ids) => queue
                    .iter()
                    .filter(|entry| !ids.contains(&entry.id))
                    .cloned()
                    .collect(),
            };
            if remaining.len() == queue.len() {
                return fresh.clone();
            }
            TaskRecord {
                pending_steering: (!remaining.is_empty()).then_some(remaining),
                ..fresh.clone()
            }
        })?;
        Ok(())
    }

    /// Drains the FRESH record's durable queue onto the now-live handle, in persisted order.
    pub fn notify_started(&self, task_id: &str) -> Result<(), SteeringError> {
        let Some(fresh) = self.try_load(task_id) else {
            return Ok(());
        };
        let Some(queue) = fresh.pending_steering.filter(|q| !q.is_empty()) else {
            return Ok(());
        };
        let Some(handle) = (self.port.live_handle)(task_id) else {
            return Ok(());
        };
        for entry in &queue {
            match deliver(handle.as_ref(), entry.deliver_as, &entry.message) {
                Ok(()) => self.event(
                    task_id,
                    "steered",
                    json!({ "delivered": deliver_as_str(entry.deliver_as), "queued": true }),
                )?,
                Err(error) => utils::logger::log(
                    "senpi-task steering queued delivery failed",
                    Some(&json!({ "taskId": task_id, "error": error.to_string() })),
                ),
            }
        }
        let ids: BTreeSet<String> = queue.into_iter().map(|entry| entry.id).collect();
        self.clear_persisted_queue(task_id, Some(&ids))
    }

    pub fn interrupt_task(&self, id_or_name: &str) -> Result<InterruptOutcome, SteeringError> {
        let Some(record) = self.resolve(id_or_name)? else {
            return Ok(InterruptOutcome::NotFound {
                reason: format!("No task found for \"{id_or_name}\"."),
            });
        };
        if record.status != TaskStatus::Running {
            return Ok(InterruptOutcome::Noop {
                reason: format!(
                    "Task {} is {}, not running.",
                    record.task_id,
                    record.status.as_str()
                ),
                task_id: record.task_id,
                status: record.status,
            });
        }
        // Transition BEFORE abort so steering is the single terminal writer.
        let result = self.port.store.transition(
            &record.task_id,
            &TaskTransition::Interrupt {
                timestamp: self.now_iso(),
                error_message: None,
                run_stats: None,
            },
        )?;
        if !result.applied {
            return Ok(InterruptOutcome::Noop {
                reason: format!(
                    "Task {} could not be interrupted from running.",
                    record.task_id
                ),
                task_id: record.task_id,
                status: result.record.status,
            });
        }
        let handle = (self.port.live_handle)(&record.task_id);
        if let Some(handle) = &handle {
            handle.abort()?;
        }
        let partial = handle.and_then(|handle| handle.last_assistant_text());
        if let Some(partial) = partial.filter(|text| !text.is_empty()) {
            self.port.store.replace(&TaskRecord {
                final_response: Some(partial),
                ..result.record
            })?;
        }
        self.event(
            &record.task_id,
            "interrupted",
            json!({ "previous_status": "running" }),
        )?;
        Ok(InterruptOutcome::Interrupted {
            task_id: record.task_id,
            previous_status: TaskStatus::Running,
        })
    }

    pub fn cancel_task(
        &self,
        id_or_name: &str,
        reason: Option<&str>,
        options: CancelOptions,
    ) -> Result<CancelOutcome, SteeringError> {
        let Some(record) = self.resolve(id_or_name)? else {
            return Ok(CancelOutcome::NotFound {
                reason: format!("No task found for \"{id_or_name}\"."),
            });
        };
        if !self.begin_send(&record.task_id) {
            return Ok(CancelOutcome::Noop {
                task_id: record.task_id.clone(),
                status: record.status,
                reason: format!("Task {} has a steering operation in progress.", record.task_id),
            });
        }
        let _lease = SendLease { active: &self.active_sends, task_id: record.task_id.clone() };
        if let Some(expected) = options.expected_run_epoch
            && record.notification.run_epoch != expected
        {
            return Ok(stale_cancel(&record));
        }
        let expected_run_epoch = options.expected_run_epoch;
        let skip_abort = options.abort == CancelAbort::Skip;
        let destruction_cause = if skip_abort {
            DestroyCause::CancelWithoutAbort
        } else {
            DestroyCause::Cancel
        };
        let cancelled_payload = |previous: &str| {
            let mut payload = json!({ "previous_status": previous });
            if let Some(reason) = reason {
                payload["reason"] = json!(reason);
            }
            payload
        };
        if record.status == TaskStatus::Pending {
            let result = self.port.store.transition_expected_epoch(
                &record.task_id,
                &TaskTransition::Cancel {
                    timestamp: self.now_iso(),
                    error_message: reason.map(str::to_string),
                    run_stats: None,
                },
                expected_run_epoch,
            )?;
            let Some(result) = result else {
                let fresh = self.port.store.load(&record.task_id)?.ok_or_else(|| {
                    SteeringError::Store(crate::store::StoreError::NotFound(record.task_id.clone()))
                })?;
                return Ok(stale_cancel(&fresh));
            };
            if !result.applied {
                return Ok(CancelOutcome::Noop {
                    reason: format!(
                        "Task {} could not be cancelled from pending.",
                        record.task_id
                    ),
                    task_id: record.task_id,
                    status: result.record.status,
                });
            }
            (self.port.dequeue_pending)(&record.task_id);
            self.clear_persisted_queue(&record.task_id, None)?;
            self.event(&record.task_id, "cancelled", cancelled_payload("pending"))?;
            self.port
                .destruction
                .destroy_resident_task(&record.task_id, destruction_cause)?;
            return Ok(CancelOutcome::Cancelled {
                task_id: record.task_id,
                previous_status: TaskStatus::Pending,
            });
        }
        if record.status != TaskStatus::Running {
            let reason_text = if record.status == TaskStatus::Cancelled {
                format!("Task {} is already cancelled.", record.task_id)
            } else {
                format!(
                    "Task {} is {}, not running.",
                    record.task_id,
                    record.status.as_str()
                )
            };
            return Ok(CancelOutcome::Noop {
                task_id: record.task_id,
                status: record.status,
                reason: reason_text,
            });
        }
        let run_stats = (self.port.run_stats_snapshot)(&record.task_id);
        let result = self.port.store.transition_expected_epoch(
            &record.task_id,
            &TaskTransition::Cancel {
                timestamp: self.now_iso(),
                error_message: reason.map(str::to_string),
                run_stats,
            },
            expected_run_epoch,
        )?;
        let Some(result) = result else {
            let fresh = self.port.store.load(&record.task_id)?.ok_or_else(|| {
                SteeringError::Store(crate::store::StoreError::NotFound(record.task_id.clone()))
            })?;
            return Ok(stale_cancel(&fresh));
        };
        if !result.applied {
            return Ok(CancelOutcome::Noop {
                reason: format!(
                    "Task {} could not be cancelled from running.",
                    record.task_id
                ),
                task_id: record.task_id,
                status: result.record.status,
            });
        }
        let handle = (self.port.live_handle)(&record.task_id);
        // abort() is best-effort: a rejection must NOT skip the destruction that moves the record
        // OUT of resident, or it would leak a residency slot forever.
        if let Some(handle) = handle.as_ref().filter(|_| !skip_abort)
            && let Err(error) = handle.abort()
        {
            utils::logger::log(
                "senpi-task steering cancel abort rejected",
                Some(&json!({ "taskId": record.task_id, "error": error.to_string() })),
            );
        }
        self.event(&record.task_id, "cancelled", cancelled_payload("running"))?;
        match handle {
            Some(handle) if skip_abort && !handle.has_terminate() => {
                self.destroy_after_settlement(handle, record.task_id.clone());
            }
            _ => self
                .port
                .destruction
                .destroy_resident_task(&record.task_id, destruction_cause)?,
        }
        Ok(CancelOutcome::Cancelled {
            task_id: record.task_id,
            previous_status: TaskStatus::Running,
        })
    }

    /// Lets an in-process child reach its exact outcome boundary before lifecycle disposes it.
    fn destroy_after_settlement(&self, handle: Arc<dyn ManagedChildHandle>, task_id: String) {
        let destruction = Arc::clone(&self.port.destruction);
        std::thread::spawn(move || {
            handle.wait_for_outcome();
            if let Err(error) =
                destruction.destroy_resident_task(&task_id, DestroyCause::CancelWithoutAbort)
            {
                utils::logger::log(
                    "senpi-task deferred cancel destruction rejected",
                    Some(&json!({ "taskId": task_id, "error": error.to_string() })),
                );
            }
        });
    }
}

fn one_shot_policy_denial(record: &TaskRecord) -> Option<SendOutcome> {
    let agent_type = record.agent_type.as_deref()?;
    let policy = interaction_policy_for_agent(agent_type).filter(|policy| policy.one_shot)?;
    Some(SendOutcome::OneShotAgent {
        task_id: record.task_id.clone(),
        agent: agent_type.to_string(),
        message: policy.send_denial_reminder.to_string(),
    })
}

fn scope_denied(record: &TaskRecord, input: &SendInput) -> Option<SendOutcome> {
    if input.all_scope {
        return None;
    }
    let caller = input.caller_session_id.as_deref()?;
    if caller == record.parent_session_id || caller == record.root_session_id {
        return None;
    }
    Some(SendOutcome::ScopeDenied {
        task_id: record.task_id.clone(),
        owning_session_id: record.parent_session_id.clone(),
        reason: format!(
            "Task {} belongs to session {}; pass all_scope to send across sessions.",
            record.task_id, record.parent_session_id
        ),
    })
}

fn not_continuable_reason(record: &TaskRecord) -> String {
    let id = &record.task_id;
    match record.residency_state {
        ResidencyState::PersistedOnly | ResidencyState::RpcDetached => {
            format!("Task {id} is suspended - resumes when its session is resumed.")
        }
        ResidencyState::Disposed => {
            format!("Task {id} was disposed and can no longer be continued.")
        }
        ResidencyState::Evicted => {
            format!("Task {id} was evicted from residency and can no longer be continued.")
        }
        ResidencyState::Resident => format!(
            "Task {id} is {} and can no longer be continued.",
            record.status.as_str()
        ),
    }
}

/// run_stats describes the FINISHED run, so it is dropped with the terminal fields.
fn build_revived(record: &TaskRecord, timestamp: String) -> TaskRecord {
    let mut revived = record.clone();
    revived.final_response = None;
    revived.error_message = None;
    revived.run_stats = None;
    revived.status = TaskStatus::Running;
    revived.residency_state = ResidencyState::Resident;
    revived.updated_at = timestamp;
    revived.notification.run_epoch += 1;
    revived
}
