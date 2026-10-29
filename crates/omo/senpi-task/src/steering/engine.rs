//! The steering engine (`steering/engine.ts`).
//!
//! Prelaunch steering is durable: sends to a still-pending child append to the record's
//! `pending_steering`, so the queue survives a restart and drains in persisted order when the
//! child launches. The record is the single source of truth - no in-memory shadow.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use super::types::{
    CANCEL_PENDING_REASON, CancelAbort, CancelOptions, CancelOutcome, DEFAULT_SEND_DELIVERY,
    InterruptOutcome, SendInput, SendOutcome, SteeringError, SteeringPort,
};
use crate::agents::interaction_policy_for_agent;
use crate::host::HostError;
use crate::lifecycle::DestroyCause;
use crate::manager::ManagedChildHandle;
use crate::shared::iso_from_ms;
use crate::state::{
    CancelRequest, DeliverAs, Messageability, PendingSteeringEntry, ResidencyState, TaskRecord,
    TaskStatus, TaskTransition, messageability,
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
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.task_id);
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
        reason: format!(
            "Task {}: the handle names an earlier run of this task.",
            record.task_id
        ),
    }
}

fn stale_cancel(record: &TaskRecord) -> CancelOutcome {
    CancelOutcome::Stale {
        task_id: record.task_id.clone(),
        status: record.status,
        run_epoch: record.notification.run_epoch,
        reason: format!(
            "Task {}: the handle names an earlier run of this task.",
            record.task_id
        ),
    }
}

impl SteeringEngine {
    pub fn new(port: SteeringPort) -> Self {
        Self {
            port,
            active_sends: Mutex::new(HashSet::new()),
        }
    }

    pub fn has_in_flight_send(&self, task_id: &str) -> bool {
        self.active_sends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(task_id)
    }

    pub fn has_pending_sends(&self, task_id: &str) -> bool {
        self.has_in_flight_send(task_id)
            || self.try_load(task_id).is_some_and(|record| {
                record
                    .pending_steering
                    .as_ref()
                    .is_some_and(|queue| !queue.is_empty())
            })
    }

    pub fn try_begin_send(&self, task_id: &str) -> bool {
        self.begin_send(task_id)
    }

    pub fn end_send(&self, task_id: &str) {
        self.active_sends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(task_id);
    }

    pub fn release_send_lease(&self, task_id: &str) {
        self.end_send(task_id);
    }

    fn begin_send(&self, task_id: &str) -> bool {
        self.active_sends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(task_id.to_string())
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
        // Same sorted order as `list`, but stops at the match and clones only that record.
        Ok(self
            .port
            .store
            .find_record(|record| record.name.as_deref() == Some(id_or_name))?)
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
                reason: format!(
                    "Task {} already has a steering operation in progress.",
                    record.task_id
                ),
                suggestion: TASK_OUTPUT_SUGGESTION.to_string(),
            });
        }
        let _lease = SendLease {
            active: &self.active_sends,
            task_id: record.task_id.clone(),
        };
        self.send_resolved(input, record)
    }

    fn send_resolved(
        &self,
        input: &SendInput,
        record: TaskRecord,
    ) -> Result<SendOutcome, SteeringError> {
        // One-shot policy runs after ownership is established but BEFORE the pending enqueue.
        if let Some(one_shot) = one_shot_policy_denial(&record) {
            return Ok(one_shot);
        }
        if let Some(outcome) = self
            .port
            .send_while_run_held
            .as_ref()
            .and_then(|send| send(&record, &input.message))
        {
            return outcome;
        }
        let deliver_as = input.deliver_as.unwrap_or(DEFAULT_SEND_DELIVERY);
        if record.status == TaskStatus::Pending {
            return self.enqueue_pending(&record, &input.message, deliver_as);
        }
        // An accepted cancel is final: nothing may revive or steer the child it is stopping (omo#9403).
        if record.status == TaskStatus::Running && record.cancel_requested.is_some() {
            return Ok(SendOutcome::NotContinuable {
                task_id: record.task_id.clone(),
                reason: format!(
                    "Task {} has a pending cancel and will not run again.",
                    record.task_id
                ),
                suggestion: TASK_OUTPUT_SUGGESTION.to_string(),
            });
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
            let fresh = self.port.store.load(&record.task_id)?.ok_or_else(|| {
                SteeringError::Store(crate::store::StoreError::NotFound(record.task_id.clone()))
            })?;
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
            if !matches!(fresh.status, TaskStatus::Pending | TaskStatus::Running)
                || fresh.killed == Some(true)
                || fresh.cancel_requested.is_some()
            {
                return fresh.clone();
            }
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
        let Some(updated) = updated else {
            return Ok(not_found(&record.task_id));
        };
        if position == 0 {
            let detail = [
                updated.error_message.as_deref(),
                updated.final_response.as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
            return Ok(SendOutcome::NotContinuable {
                reason: if detail.is_empty() {
                    format!("Task {} is {}.", updated.task_id, updated.status.as_str())
                } else {
                    detail
                },
                task_id: updated.task_id,
                suggestion: TASK_OUTPUT_SUGGESTION.to_string(),
            });
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
        if !matches!(fresh.status, TaskStatus::Pending | TaskStatus::Running)
            || fresh.killed == Some(true)
            || fresh.cancel_requested.is_some()
        {
            return Ok(());
        }
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
        // An accepted cancel owns this run's ending: an interrupt cannot turn it into a resumable stop.
        if record.cancel_requested.is_some() {
            return Ok(InterruptOutcome::Noop {
                reason: format!(
                    "Task {} has a pending cancel; it ends cancelled.",
                    record.task_id
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
                reason: format!(
                    "Task {} has a steering operation in progress.",
                    record.task_id
                ),
            });
        }
        let _lease = SendLease {
            active: &self.active_sends,
            task_id: record.task_id.clone(),
        };
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
        if !matches!(record.status, TaskStatus::Running | TaskStatus::Paused) {
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
        let reachable = (self.port.live_handle)(&record.task_id);
        let recovering = reachable
            .as_ref()
            .is_some_and(|handle| handle.transport_recovering());
        // The stop already waiting on this child's connection is the cancel; a repeat joins it.
        if record.cancel_requested.is_some() && recovering {
            return Ok(cancel_pending(&record));
        }
        if !skip_abort
            && recovering
            && let Some(reachable) =
                reachable.filter(|handle| handle.supports_stop_when_reachable())
        {
            return self.stop_when_reachable(&record, reachable, reason, expected_run_epoch);
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
        // From here no transport recovery may bring the child back: a host that crashes before the stop
        // lands is reached again only to end the session (omo#9403).
        if let Some(handle) = handle.as_ref() {
            handle.mark_stopping();
        }
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
        self.event(
            &record.task_id,
            "cancelled",
            cancelled_payload(record.status.as_str()),
        )?;
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
            previous_status: record.status,
        })
    }

    /// omo#9403: a child whose connection is down cannot be told to stop, and calling it cancelled would
    /// be false - it may still be running on its host. The stop waits on the handle (applied on the host
    /// before anything else once reachable, or the child ends when its connection never comes back), and
    /// only then is the record cancelled and the child torn down, which releases its lane.
    fn stop_when_reachable(
        &self,
        record: &TaskRecord,
        handle: Arc<dyn ManagedChildHandle>,
        reason: Option<&str>,
        expected: Option<i64>,
    ) -> Result<CancelOutcome, SteeringError> {
        // Durable first: a parent that shuts down or crashes before the stop lands leaves the cancel on the
        // record, and every revival finishes it instead of running the child again.
        let requested_at = self.now_iso();
        let mut moved: Option<CancelOutcome> = None;
        self.port.store.mutate(&record.task_id, |fresh| {
            if let Some(expected) = expected
                && fresh.notification.run_epoch != expected
            {
                moved = Some(stale_cancel(fresh));
                return fresh.clone();
            }
            TaskRecord {
                cancel_requested: Some(CancelRequest {
                    requested_at: requested_at.clone(),
                    reason: reason.map(str::to_string),
                }),
                ..fresh.clone()
            }
        })?;
        if let Some(moved) = moved {
            return Ok(moved);
        }
        if let Some(stop_requested) = &self.port.stop_requested {
            stop_requested(&record.task_id);
        }
        let mut payload = json!({ "unreachable": true });
        if let Some(reason) = reason {
            payload["reason"] = json!(reason);
        }
        self.event(&record.task_id, "cancel_requested", payload)?;
        // The pending stop is settled on every path, a failed record write included: the outcome tracker
        // waits on that settlement and ends the run as cancelled itself when this write did not land.
        let port = self.port.clone();
        let task_id = record.task_id.clone();
        let error_message = reason.map(str::to_string);
        let reason = reason.map(str::to_string);
        std::thread::spawn(move || {
            if let Err(error) = handle.stop_when_reachable() {
                utils::logger::log(
                    "senpi-task deferred cancel of an unreachable child failed",
                    Some(&json!({ "taskId": task_id, "error": error.to_string() })),
                );
            }
            let engine = SteeringEngine::new(port);
            if let Err(error) = engine.finish_stop(&task_id, error_message, reason) {
                utils::logger::log(
                    "senpi-task deferred cancel of an unreachable child failed",
                    Some(&json!({ "taskId": task_id, "error": error.to_string() })),
                );
            }
            if let Some(stop_settled) = &engine.port.stop_settled {
                stop_settled(&task_id);
            }
        });
        Ok(cancel_pending(record))
    }

    /// The `finish` step of the deferred stop: cancel the record, then tear the child down.
    fn finish_stop(
        &self,
        task_id: &str,
        error_message: Option<String>,
        reason: Option<String>,
    ) -> Result<(), SteeringError> {
        let previous_status = self.port.store.load(task_id)?.map(|record| record.status).unwrap_or(TaskStatus::Running);
        let run_stats = (self.port.run_stats_snapshot)(task_id);
        let result = self.port.store.transition(
            task_id,
            &TaskTransition::Cancel {
                timestamp: self.now_iso(),
                error_message,
                run_stats,
            },
        )?;
        if result.applied {
            self.clear_persisted_queue(task_id, None)?;
            let mut payload = json!({ "previous_status": previous_status.as_str() });
            if let Some(reason) = reason {
                payload["reason"] = json!(reason);
            }
            self.event(task_id, "cancelled", payload)?;
        }
        self.port
            .destruction
            .destroy_resident_task(task_id, DestroyCause::Cancel)?;
        Ok(())
    }

    /// Lets an in-process child reach its exact outcome boundary before lifecycle disposes it.
    fn destroy_after_settlement(&self, handle: Arc<dyn ManagedChildHandle>, task_id: String) {
        let destruction = Arc::clone(&self.port.destruction);
        #[cfg(test)]
        crate::manager::outcome::test_barrier::grant_spawned();
        std::thread::spawn(move || {
            #[cfg(test)]
            let _done = crate::manager::outcome::test_barrier::GrantDone;
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

fn cancel_pending(record: &TaskRecord) -> CancelOutcome {
    CancelOutcome::CancelPending {
        task_id: record.task_id.clone(),
        previous_status: record.status,
        reason: CANCEL_PENDING_REASON.to_string(),
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

fn fold_prior_runs_cost(record: &TaskRecord) -> Option<f64> {
    let finished = record
        .run_stats
        .as_ref()
        .filter(|stats| {
            matches!(
                stats.cost_status,
                Some(
                    crate::state::CostReportStatus::Reported
                        | crate::state::CostReportStatus::Partial
                )
            )
        })
        .and_then(|stats| stats.cost_usd)
        .filter(|cost| cost.is_finite() && *cost > 0.0)
        .unwrap_or(0.0);
    let total = record.prior_runs_cost_usd.unwrap_or(0.0) + finished;
    (total > 0.0).then_some(total)
}

/// run_stats describes the FINISHED run, so it is dropped with the terminal fields.
pub(super) fn build_revived(record: &TaskRecord, timestamp: String) -> TaskRecord {
    let mut revived = record.clone();
    revived.final_response = None;
    revived.error_message = None;
    revived.prior_runs_cost_usd = fold_prior_runs_cost(record);
    revived.run_stats = None;
    revived.failure_kind = None;
    revived.failure_reason = None;
    revived.awaiting_input = None;
    revived.status = TaskStatus::Running;
    revived.residency_state = ResidencyState::Resident;
    revived.updated_at = timestamp;
    revived.notification.run_epoch += 1;
    revived.run_start_epoch = revived.notification.run_epoch;
    revived
}
