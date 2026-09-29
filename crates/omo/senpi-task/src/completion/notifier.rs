use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde_json::json;

use super::notification::{
    BuildDetailsOptions, build_completion_details, build_completion_message,
};
use super::routing::{route_completion, should_notify_status};
use super::types::{
    CompletionDetails, CompletionNotifierDeps, CompletionNotifierStore, CompletionRequest,
    CompletionRetrySchedule, DeliveredDecision, FlushInput, FlushResult, NotifyResult,
    ParentNotifier, ParentNotifierMessage, ParentState, ReconcileUnnotifiedNotificationsInput,
    RoutingDecision, ScheduledCancel, ScheduledTask, SkipReason,
};
use crate::host::HostError;
use crate::state::{TaskRecord, TaskStatus};
use crate::store::{PersistedTaskEvent, StoreError};

const MAX_SCHEDULED_RETRIES: u32 = 8;
const RETRY_BASE_MS: u64 = 500;
const RETRY_MAX_MS: u64 = 30_000;
const RETRY_JITTER_MS: u64 = 200;

#[derive(Debug, Clone)]
struct BufferedEntry {
    task_id: String,
    epoch: i64,
    details: CompletionDetails,
}

#[derive(Default)]
struct NotifierState {
    buffered: HashMap<String, Vec<BufferedEntry>>,
    scheduled_retries: HashMap<String, ScheduledCancel>,
    scheduled_retry_counts: HashMap<String, u32>,
}

struct Inner {
    deps: CompletionNotifierDeps,
    schedule: CompletionRetrySchedule,
    state: Mutex<NotifierState>,
}

/// Pushes terminal child completions into the parent session, buffering across transient parent
/// states and retrying failed enqueues on a capped backoff ladder. Dedupe identity is
/// `(task_id, run_epoch)`.
#[derive(Clone)]
pub struct CompletionNotifier {
    inner: Arc<Inner>,
}

pub fn create_completion_notifier(deps: CompletionNotifierDeps) -> CompletionNotifier {
    let schedule = deps.schedule.clone().unwrap_or_else(default_schedule);
    CompletionNotifier {
        inner: Arc::new(Inner {
            deps,
            schedule,
            state: Mutex::new(NotifierState::default()),
        }),
    }
}

impl CompletionNotifier {
    pub fn notify_terminal(&self, request: &CompletionRequest) -> Result<NotifyResult, StoreError> {
        if !request.run_in_background {
            return Ok(NotifyResult::Skipped(SkipReason::SyncTask));
        }
        let record = self
            .inner
            .deps
            .store
            .load(&request.record.task_id)?
            .unwrap_or_else(|| request.record.clone());
        if !is_terminal(record.status) {
            return Ok(NotifyResult::Skipped(SkipReason::NotTerminal));
        }
        if !should_notify_status(record.status) {
            return Ok(NotifyResult::Skipped(SkipReason::NonNotifyingTerminal));
        }
        if record.notification.notified_epoch >= record.notification.run_epoch {
            return Ok(NotifyResult::Skipped(SkipReason::AlreadyNotified));
        }
        let details = self.inner.build_details(&record, request.tokens);
        Inner::deliver_record(&self.inner, &record, details, request.parent_state)
    }

    pub fn flush_buffered(&self, input: &FlushInput) -> Result<FlushResult, StoreError> {
        let entries = self.inner.lock().buffered.remove(&input.session_id);
        let Some(entries) = entries.filter(|entries| !entries.is_empty()) else {
            return Ok(FlushResult::Empty);
        };
        let store = self.inner.deps.store.as_ref();
        if input.replaced {
            for entry in &entries {
                drop_entry(store, entry)?;
            }
            return Ok(FlushResult::Dropped(entries.len()));
        }
        let details: Vec<CompletionDetails> =
            entries.iter().map(|entry| entry.details.clone()).collect();
        let mut message = build_completion_message(&details);
        message.trigger_turn = Some(true);
        if let Err(error) = deliver_with_retry(self.inner.deps.notifier.as_ref(), &message) {
            for entry in &entries {
                if store.load(&entry.task_id)?.is_some() {
                    record_failure(store, &entry.task_id, entry.epoch, &error)?;
                }
            }
            return Ok(FlushResult::Failed(entries.len()));
        }
        for entry in &entries {
            persist_notified(store, &entry.task_id, entry.epoch)?;
        }
        Ok(FlushResult::Flushed(entries.len()))
    }

    pub fn buffered_count(&self, session_id: &str) -> usize {
        self.inner
            .lock()
            .buffered
            .get(session_id)
            .map_or(0, Vec::len)
    }

    /// Crash recovery: the in-memory buffer dies with the process, so on session start every
    /// terminal child of THIS session that still owes a notification goes through the normal
    /// delivery path. Two populations owe one: notify_on_terminal records whose latest run_epoch
    /// was never notified, and legacy records with an in-flight failed delivery.
    pub fn reconcile_unnotified_notifications(
        &self,
        input: ReconcileUnnotifiedNotificationsInput<'_>,
    ) -> Result<(), StoreError> {
        let listed = self.inner.deps.store.list()?;
        for record in listed.records {
            let epoch = record.notification.run_epoch;
            if record.parent_session_id != input.session_id
                || record.notification.notified_epoch >= epoch
                || !is_terminal(record.status)
                || !should_notify_status(record.status)
                || !owes_notification(&record)
            {
                continue;
            }
            // A live buffered entry already owns delivery of this (task_id, run_epoch); the next
            // flush delivers it, so delivering here too would double-notify.
            if self
                .inner
                .has_buffered(&record.parent_session_id, &record.task_id, epoch)
            {
                continue;
            }
            let details = self.inner.build_details(&record, None);
            Inner::deliver_record(&self.inner, &record, details, input.parent_state)?;
        }
        Ok(())
    }

    pub fn reconcile_failed_notifications(
        &self,
        input: ReconcileUnnotifiedNotificationsInput<'_>,
    ) -> Result<(), StoreError> {
        self.reconcile_unnotified_notifications(input)
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, NotifierState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn build_details(&self, record: &TaskRecord, tokens: Option<u64>) -> CompletionDetails {
        build_completion_details(
            record,
            BuildDetailsOptions {
                tokens,
                state_dir: self.deps.state_dir.as_deref(),
            },
        )
    }

    fn has_buffered(&self, session_id: &str, task_id: &str, epoch: i64) -> bool {
        self.lock().buffered.get(session_id).is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.task_id == task_id && entry.epoch == epoch)
        })
    }

    // Defense-in-depth dedupe: a buffered entry is not persisted until flush, so two notifications
    // for the same (task_id, epoch) before a flush must buffer once.
    fn push_buffered(&self, session_id: &str, entry: BufferedEntry) {
        let mut state = self.lock();
        let existing = state.buffered.entry(session_id.to_string()).or_default();
        if existing
            .iter()
            .any(|buffered| buffered.task_id == entry.task_id && buffered.epoch == entry.epoch)
        {
            return;
        }
        existing.push(entry);
    }

    fn finish_retry_chain(&self, entry: &BufferedEntry) {
        let key = retry_key(entry);
        let cancel = {
            let mut state = self.lock();
            state.scheduled_retry_counts.remove(&key);
            state.scheduled_retries.remove(&key)
        };
        if let Some(cancel) = cancel {
            cancel();
        }
    }

    fn schedule_retry(this: &Arc<Self>, entry: BufferedEntry) {
        let key = retry_key(&entry);
        let retry_number = {
            let state = this.lock();
            if state.scheduled_retries.contains_key(&key) {
                return;
            }
            state.scheduled_retry_counts.get(&key).copied().unwrap_or(0) + 1
        };
        if retry_number > MAX_SCHEDULED_RETRIES {
            // Exhausted the ladder: drop the retry state so a later reconcile restarts fresh.
            this.finish_retry_chain(&entry);
            return;
        }
        this.lock()
            .scheduled_retry_counts
            .insert(key.clone(), retry_number);
        let weak: Weak<Self> = Arc::downgrade(this);
        let timer_key = key.clone();
        let task: ScheduledTask = Box::new(move || {
            let Some(inner) = weak.upgrade() else { return };
            let cancel = inner.lock().scheduled_retries.remove(&timer_key);
            drop(cancel);
            if let Err(error) = Self::run_scheduled_retry(&inner, entry) {
                utils::logger::log(
                    "senpi-task completion retry failed",
                    Some(&json!({ "error": error.to_string() })),
                );
            }
        });
        let cancel = (this.schedule)(task, retry_delay(retry_number));
        // A synchronous scheduler may already have run (and finished) the chain; only a still-open
        // chain owns the cancel handle.
        let mut state = this.lock();
        if state.scheduled_retry_counts.get(&key) == Some(&retry_number)
            && !state.scheduled_retries.contains_key(&key)
        {
            state.scheduled_retries.insert(key, cancel);
        }
    }

    fn run_scheduled_retry(this: &Arc<Self>, entry: BufferedEntry) -> Result<(), StoreError> {
        let Some(fresh) = this.deps.store.load(&entry.task_id)? else {
            this.finish_retry_chain(&entry);
            return Ok(());
        };
        if fresh.notification.run_epoch != entry.epoch
            || !is_terminal(fresh.status)
            || !should_notify_status(fresh.status)
            || fresh.notification.notified_epoch >= entry.epoch
        {
            this.finish_retry_chain(&entry);
            return Ok(());
        }
        let parent_state = this
            .deps
            .get_parent_state
            .as_ref()
            .map_or(ParentState::Idle, |get| get());
        let decision = route_completion(parent_state);
        let current_session = this
            .deps
            .get_current_session_id
            .as_ref()
            .and_then(|get| get());
        if current_session.as_deref() != Some(fresh.parent_session_id.as_str()) {
            this.finish_retry_chain(&entry);
            return Ok(());
        }
        if let RoutingDecision::Buffer(_) = decision {
            this.push_buffered(&fresh.parent_session_id, entry.clone());
            this.finish_retry_chain(&entry);
            return Ok(());
        }
        let message = build_delivery_message(std::slice::from_ref(&entry.details));
        match deliver_with_retry(this.deps.notifier.as_ref(), &message) {
            Ok(()) => {
                this.finish_retry_chain(&entry);
                persist_notified(this.deps.store.as_ref(), &fresh.task_id, entry.epoch)
            }
            Err(_) => {
                Self::schedule_retry(this, entry);
                Ok(())
            }
        }
    }

    fn deliver_record(
        this: &Arc<Self>,
        record: &TaskRecord,
        details: CompletionDetails,
        parent_state: ParentState,
    ) -> Result<NotifyResult, StoreError> {
        let entry = BufferedEntry {
            task_id: record.task_id.clone(),
            epoch: record.notification.run_epoch,
            details,
        };
        let decision = route_completion(parent_state);
        let delivered = match decision {
            RoutingDecision::Buffer(reason) => {
                this.push_buffered(&record.parent_session_id, entry);
                return Ok(NotifyResult::Buffered(reason));
            }
            RoutingDecision::Wake => DeliveredDecision::Wake,
            RoutingDecision::DeliverStreaming => DeliveredDecision::DeliverStreaming,
        };
        let message = build_delivery_message(std::slice::from_ref(&entry.details));
        match deliver_with_retry(this.deps.notifier.as_ref(), &message) {
            Ok(()) => {
                this.finish_retry_chain(&entry);
                persist_notified(this.deps.store.as_ref(), &record.task_id, entry.epoch)?;
                Ok(NotifyResult::Delivered(delivered))
            }
            Err(error) => {
                record_failure(
                    this.deps.store.as_ref(),
                    &record.task_id,
                    entry.epoch,
                    &error,
                )?;
                Self::schedule_retry(this, entry);
                Ok(NotifyResult::Failed)
            }
        }
    }
}

fn is_terminal(status: TaskStatus) -> bool {
    status.is_terminal()
}

fn owes_notification(record: &TaskRecord) -> bool {
    record.notify_on_terminal || record.notification.notification_failed_epoch.is_some()
}

// Every delivered notification stamps trigger_turn so the host batches ALL ready notifications into
// ONE injection steered into the running turn (unconditional-steer contract).
fn build_delivery_message(details: &[CompletionDetails]) -> ParentNotifierMessage {
    let mut message = build_completion_message(details);
    message.trigger_turn = Some(true);
    message
}

fn default_schedule() -> CompletionRetrySchedule {
    Arc::new(|task: ScheduledTask, delay_ms: u64| -> ScheduledCancel {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            if !flag.load(Ordering::SeqCst) {
                task();
            }
        });
        Box::new(move || cancelled.store(true, Ordering::SeqCst))
    })
}

fn retry_delay(retry_number: u32) -> u64 {
    let exponent = retry_number.saturating_sub(1).min(8);
    let backoff_ms = RETRY_BASE_MS * 2_u64.pow(exponent);
    RETRY_MAX_MS.min(backoff_ms + jitter_ms())
}

fn jitter_ms() -> u64 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    hasher.finish() % RETRY_JITTER_MS
}

fn retry_key(entry: &BufferedEntry) -> String {
    format!("{}:{}", entry.task_id, entry.epoch)
}

fn deliver_with_retry(
    notifier: &dyn ParentNotifier,
    message: &ParentNotifierMessage,
) -> Result<(), HostError> {
    notifier
        .enqueue(message)
        .or_else(|_| notifier.enqueue(message))
}

// Epoch-only bookkeeping through a conditional mutate: the locked re-read keeps every other field,
// so a concurrent residency/host_pid claim is never clobbered by a stale whole-record replace.
fn persist_notified(
    store: &dyn CompletionNotifierStore,
    task_id: &str,
    epoch: i64,
) -> Result<(), StoreError> {
    store.mutate(task_id, &mut |fresh| {
        let mut next = fresh.clone();
        if fresh.notification.notified_epoch < epoch {
            next.notification.notified_epoch = epoch;
        }
        next
    })?;
    Ok(())
}

fn record_failure(
    store: &dyn CompletionNotifierStore,
    task_id: &str,
    epoch: i64,
    error: &HostError,
) -> Result<(), StoreError> {
    store.append_event(
        task_id,
        &PersistedTaskEvent {
            event_type: "notification_failed".to_string(),
            payload: json!({ "epoch": epoch, "error": error.to_string() }),
        },
    )?;
    store.mutate(task_id, &mut |fresh| {
        let mut next = fresh.clone();
        if fresh.notification.notified_epoch < epoch
            && fresh.notification.notification_failed_epoch != Some(epoch)
        {
            next.notification.notification_failed_epoch = Some(epoch);
        }
        next
    })?;
    utils::logger::log(
        "senpi-task completion delivery failed",
        Some(&json!({ "taskId": task_id, "epoch": epoch })),
    );
    Ok(())
}

fn drop_entry(
    store: &dyn CompletionNotifierStore,
    entry: &BufferedEntry,
) -> Result<(), StoreError> {
    store.append_event(
        &entry.task_id,
        &PersistedTaskEvent {
            event_type: "notification_dropped".to_string(),
            payload: json!({ "epoch": entry.epoch }),
        },
    )?;
    utils::logger::log(
        "senpi-task completion dropped for replaced session",
        Some(&json!({ "taskId": entry.task_id, "epoch": entry.epoch })),
    );
    Ok(())
}
