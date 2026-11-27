//! Unhandled host-session cancel (`steering/host-session-cancel.ts`, `#11765`, `#11783`).

use std::sync::Arc;

use serde_json::json;

use super::types::{CancelOutcome, SteeringError, SteeringPort};
use crate::lifecycle::DestroyCause;
use crate::lifecycle::host_session::{HostSessionCancelSink, HostSessionEnd};
use crate::state::{CancelRequest, TaskRecord, TaskTransition};
use crate::store::PersistedTaskEvent;

struct SinkImpl {
    port: SteeringPort,
    record: TaskRecord,
    reason: Option<String>,
    error_message: Option<String>,
    clear_persisted_queue: Arc<dyn Fn(&str) + Send + Sync>,
}

impl SinkImpl {
    fn finish(&self) -> Result<bool, SteeringError> {
        let run_stats = (self.port.run_stats_snapshot)(&self.record.task_id);
        let result = self.port.store.transition(
            &self.record.task_id,
            &TaskTransition::Cancel {
                timestamp: crate::shared::iso_from_ms((self.port.now)()),
                error_message: self.error_message.clone(),
                run_stats,
            },
        )?;
        if !result.applied {
            return Ok(false);
        }
        (self.clear_persisted_queue)(&self.record.task_id);
        let mut payload = json!({ "previous_status": self.record.status.as_str() });
        if let Some(reason) = &self.reason {
            payload["reason"] = json!(reason);
        }
        self.port.store.append_event(
            &self.record.task_id,
            &PersistedTaskEvent {
                event_type: "cancelled".to_string(),
                payload,
            },
        )?;
        self.port
            .destruction
            .destroy_resident_task(&self.record.task_id, DestroyCause::Cancel)?;
        Ok(true)
    }
}

impl HostSessionCancelSink for SinkImpl {
    fn ended(&self) {
        if let Err(error) = self.finish() {
            utils::logger::log(
                "senpi-task host session cancel finish failed",
                Some(&json!({ "taskId": self.record.task_id, "error": error.to_string() })),
            );
        }
    }

    fn settle(&self) {
        if let Some(stop_settled) = &self.port.stop_settled {
            stop_settled(&self.record.task_id);
        }
    }
}

pub(super) fn cancel_unhandled_host_session(
    port: &SteeringPort,
    record: &TaskRecord,
    reason: Option<&str>,
    expected_run_epoch: Option<i64>,
    cancel_pending: impl Fn(&TaskRecord, &str) -> CancelOutcome,
    clear_persisted_queue: Arc<dyn Fn(&str) + Send + Sync>,
    stale_cancel: impl Fn(&TaskRecord) -> CancelOutcome,
) -> Result<CancelOutcome, SteeringError> {
    if record.cancel_requested.is_some() {
        return Ok(cancel_pending(record, super::types::CANCEL_PENDING_REASON));
    }
    let requested_at = crate::shared::iso_from_ms((port.now)());
    let mut moved: Option<CancelOutcome> = None;
    port.store.mutate(&record.task_id, |fresh| {
        if let Some(expected) = expected_run_epoch
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
    let mut payload = json!({ "unreachable": true });
    if let Some(reason) = reason {
        payload["reason"] = json!(reason);
    }
    port.store.append_event(
        &record.task_id,
        &PersistedTaskEvent {
            event_type: "cancel_requested".to_string(),
            payload,
        },
    )?;

    let Some(cancel) = port.destruction.host_session_cancel() else {
        return Ok(cancel_pending(record, super::types::CANCEL_PENDING_REASON));
    };
    if let Some(stop_requested) = &port.stop_requested {
        stop_requested(&record.task_id);
    }

    let sink = Arc::new(SinkImpl {
        port: port.clone(),
        record: record.clone(),
        reason: reason.map(str::to_string),
        error_message: reason.map(str::to_string),
        clear_persisted_queue,
    });

    let end = (cancel.end)(record);
    if matches!(end, HostSessionEnd::Ended { .. }) {
        let applied = sink.finish();
        sink.settle();
        let applied = applied?;
        if !applied {
            let status = port
                .store
                .load(&record.task_id)?
                .map_or(record.status, |fresh| fresh.status);
            return Ok(CancelOutcome::Noop {
                task_id: record.task_id.clone(),
                status,
                reason: format!(
                    "Task {} is {}, not running.",
                    record.task_id,
                    status.as_str()
                ),
            });
        }
        return Ok(CancelOutcome::Cancelled {
            task_id: record.task_id.clone(),
            previous_status: record.status,
        });
    }

    let reason_text = match &end {
        HostSessionEnd::NotEnded { reason } => reason.clone(),
        HostSessionEnd::Ended { .. } => super::types::CANCEL_PENDING_REASON.to_string(),
    };
    (cancel.drive)(record.clone(), sink);
    Ok(cancel_pending(record, &reason_text))
}
