//! `dag/infrastructure-retry.ts`: automatic retry of DAG nodes whose child failed on typed
//! infrastructure (never on an ordinary child error), with bounded exponential backoff.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::dag::journal::DagJournal;
use crate::dag::manager::DagRunRecordV1;
use crate::dag::node_retry::reset_dag_nodes;
use crate::dag::scheduler::DagSchedulerOptions;
use crate::dag::store::{DagEventReadOptions, DagStoreError};
use crate::dag::types::{
    DagDiagnostic, DagNodeId, DagNodeRetryAutomatic, DagNodeState, DagRunEventPayload,
    DagRunEventType,
};
use crate::state::{TaskRecord, TaskStatus};

pub const MAX_INFRASTRUCTURE_RETRIES: u32 = 2;
pub const INITIAL_BACKOFF_MS: u64 = 1_000;

/// Cancels a scheduled retry timer.
pub type RetryCancel = Box<dyn FnOnce() + Send>;
/// `(callback, delay_ms) -> cancel`: injectable timer for the retry backoff.
pub type RetrySchedule = Arc<dyn Fn(Box<dyn FnOnce() + Send>, u64) -> RetryCancel + Send + Sync>;

/// Only typed infrastructure failures qualify; ordinary child errors never enter this path.
pub fn is_infrastructure_failure(task: &TaskRecord) -> bool {
    if task.cancel_requested.is_some() {
        return false;
    }
    if !matches!(task.status, TaskStatus::Error | TaskStatus::Lost) {
        return false;
    }
    match task.failure_kind.as_deref() {
        Some("child-unresponsive" | "host_unavailable" | "suspended_unresumable") => true,
        Some("session_unavailable") => matches!(
            task.failure_reason.as_deref(),
            Some("host_draining" | "host_memory_pressure")
        ),
        _ => false,
    }
}

struct Inner {
    counts: BTreeMap<DagNodeId, u32>,
    timers: BTreeMap<DagNodeId, Option<RetryCancel>>,
    ready: bool,
    disposed: bool,
}

pub struct InfrastructureRetries {
    inner: Arc<Mutex<Inner>>,
    journal: Arc<DagJournal<DagRunRecordV1>>,
    schedule: RetrySchedule,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

fn lock(inner: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn create_infrastructure_retries(
    options: &DagSchedulerOptions,
    journal: Arc<DagJournal<DagRunRecordV1>>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<InfrastructureRetries, DagStoreError> {
    let schedule = options
        .schedule_retry
        .clone()
        .unwrap_or_else(|| Arc::new(schedule_timer));
    let mut counts: BTreeMap<DagNodeId, u32> = BTreeMap::new();
    let mut latest: BTreeMap<DagNodeId, (u32, String, u64)> = BTreeMap::new();
    let run_id = journal.snapshot().run_id;
    let mut since = 0;
    // Counts belong to the durable run, not a scheduler instance or a display attempt.
    loop {
        let page = options.store.read_events(
            &run_id,
            since,
            &DagEventReadOptions {
                limit: 1000,
                types: Some(vec![DagRunEventType::NodeRetried]),
                ..Default::default()
            },
        )?;
        for event in &page.events {
            if let DagRunEventPayload::NodeRetried {
                node_id,
                exec_attempt,
                automatic,
                ..
            } = &event.payload
            {
                let delay = automatic.as_ref().map(|automatic| automatic.delay_ms);
                latest.insert(
                    node_id.clone(),
                    (*exec_attempt, event.at.clone(), delay.unwrap_or(u64::MAX)),
                );
                if automatic.is_some() {
                    *counts.entry(node_id.clone()).or_default() += 1;
                } else {
                    // Distinguish a manual retry as the latest event: no timer is re-armed for it.
                    latest.insert(node_id.clone(), (*exec_attempt, event.at.clone(), u64::MAX));
                }
            }
        }
        match page.events.last() {
            Some(last) if page.has_more => since = last.seq,
            _ => break,
        }
    }
    let retries = InfrastructureRetries {
        inner: Arc::new(Mutex::new(Inner {
            counts,
            timers: BTreeMap::new(),
            ready: false,
            disposed: false,
        })),
        journal: Arc::clone(&journal),
        schedule,
        now: Arc::clone(&now),
        wake,
    };
    for node in journal.snapshot().nodes {
        let Some((exec_attempt, at, delay_ms)) = latest.get(&node.id) else {
            continue;
        };
        if node.state != DagNodeState::Pending
            || *delay_ms == u64::MAX
            || Some(*exec_attempt) != node.exec_attempt
        {
            continue;
        }
        let at_ms =
            chrono::DateTime::parse_from_rfc3339(at).map_or(0, |parsed| parsed.timestamp_millis());
        let remaining =
            (at_ms.saturating_add(i64::try_from(*delay_ms).unwrap_or(i64::MAX)) - now()).max(0);
        retries.arm(&node.id, u64::try_from(remaining).unwrap_or(0));
    }
    Ok(retries)
}

impl InfrastructureRetries {
    fn arm(&self, node_id: &DagNodeId, delay_ms: u64) {
        lock(&self.inner).timers.insert(node_id.clone(), None);
        let inner = Arc::clone(&self.inner);
        let wake = Arc::clone(&self.wake);
        let id = node_id.clone();
        let cancel = (self.schedule)(
            Box::new(move || {
                {
                    let mut state = lock(&inner);
                    state.timers.remove(&id);
                    if state.disposed {
                        return;
                    }
                    state.ready = true;
                }
                wake();
            }),
            delay_ms,
        );
        let mut state = lock(&self.inner);
        // The timer may already have fired (and removed itself) while scheduling.
        if let Some(slot) = state.timers.get_mut(node_id) {
            *slot = Some(cancel);
        }
    }

    /// Resets the node for a fresh attempt after a typed infrastructure failure and arms the backoff.
    pub fn retry(&self, node_id: &DagNodeId, task: &TaskRecord) -> Result<(), DagStoreError> {
        let (attempt, delay_ms, failure_kind) = {
            let state = lock(&self.inner);
            if state.disposed || !is_infrastructure_failure(task) {
                return Ok(());
            }
            let count = state.counts.get(node_id).copied().unwrap_or(0);
            let Some(failure_kind) = task.failure_kind.clone() else {
                return Ok(());
            };
            if count >= MAX_INFRASTRUCTURE_RETRIES {
                return Ok(());
            }
            (
                count + 1,
                INITIAL_BACKOFF_MS * 2u64.pow(count),
                failure_kind,
            )
        };
        reset_dag_nodes(
            &self.journal,
            std::slice::from_ref(node_id),
            false,
            Some(DagNodeRetryAutomatic {
                attempt,
                failure_kind,
                delay_ms,
            }),
        )?;
        lock(&self.inner).counts.insert(node_id.clone(), attempt);
        self.journal.append(DagRunEventPayload::DiagnosticAdded {
            diagnostic: DagDiagnostic::NodeFlag {
                node_id: node_id.clone(),
                message: format!(
                    "retried after infra failure ({attempt}/{MAX_INFRASTRUCTURE_RETRIES})"
                ),
                at: crate::shared::iso_from_ms((self.now)()),
            },
        })?;
        self.arm(node_id, delay_ms);
        Ok(())
    }

    pub fn pending(&self, node_id: Option<&DagNodeId>) -> bool {
        let state = lock(&self.inner);
        node_id.map_or_else(
            || !state.timers.is_empty(),
            |id| state.timers.contains_key(id),
        )
    }

    pub fn take_ready(&self) -> bool {
        std::mem::take(&mut lock(&self.inner).ready)
    }

    pub fn cancel_pending_timers(&self) {
        let cancels: Vec<RetryCancel> = {
            let mut state = lock(&self.inner);
            state.disposed = true;
            std::mem::take(&mut state.timers)
                .into_values()
                .flatten()
                .collect()
        };
        for cancel in cancels {
            cancel();
        }
    }
}

/// Default retry timer: a thread that sleeps on a condvar so cancellation is prompt.
fn schedule_timer(callback: Box<dyn FnOnce() + Send>, delay_ms: u64) -> RetryCancel {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let thread_gate = Arc::clone(&gate);
    let spawned = std::thread::Builder::new()
        .name("dag-infra-retry".into())
        .spawn(move || {
            let (flag, condvar) = &*thread_gate;
            let deadline = std::time::Instant::now() + Duration::from_millis(delay_ms);
            let mut cancelled = flag.lock().unwrap_or_else(PoisonError::into_inner);
            while !*cancelled {
                let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                    break;
                };
                if left.is_zero() {
                    break;
                }
                cancelled = condvar
                    .wait_timeout(cancelled, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            if !*cancelled {
                drop(cancelled);
                callback();
            }
        });
    Box::new(move || {
        let (flag, condvar) = &*gate;
        *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        condvar.notify_all();
        drop(spawned);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(status: TaskStatus, kind: Option<&str>, reason: Option<&str>) -> TaskRecord {
        let mut record = crate::test_support::base_record("st_infra", "parent");
        record.status = status;
        record.failure_kind = kind.map(str::to_string);
        record.failure_reason = reason.map(str::to_string);
        record
    }

    #[test]
    fn only_typed_infrastructure_failures_qualify() {
        for kind in [
            "child-unresponsive",
            "host_unavailable",
            "suspended_unresumable",
        ] {
            assert!(is_infrastructure_failure(&record(
                TaskStatus::Error,
                Some(kind),
                None
            )));
            assert!(is_infrastructure_failure(&record(
                TaskStatus::Lost,
                Some(kind),
                None
            )));
            assert!(!is_infrastructure_failure(&record(
                TaskStatus::Cancelled,
                Some(kind),
                None
            )));
        }
        assert!(is_infrastructure_failure(&record(
            TaskStatus::Error,
            Some("session_unavailable"),
            Some("host_draining")
        )));
        assert!(is_infrastructure_failure(&record(
            TaskStatus::Error,
            Some("session_unavailable"),
            Some("host_memory_pressure")
        )));
        assert!(!is_infrastructure_failure(&record(
            TaskStatus::Error,
            Some("session_unavailable"),
            Some("other")
        )));
        for kind in [
            "child-prompt-failed",
            "child-turn-failed",
            "session-create-failed",
            "depth-exceeded",
            "model_unavailable",
            "tools_unavailable",
        ] {
            assert!(!is_infrastructure_failure(&record(
                TaskStatus::Error,
                Some(kind),
                None
            )));
        }
        assert!(!is_infrastructure_failure(&record(
            TaskStatus::Error,
            None,
            None
        )));
    }
}
