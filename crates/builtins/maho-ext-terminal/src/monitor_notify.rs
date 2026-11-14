use crate::monitor_notify_format::{
    WAKE_BUDGET_PAUSE_NOTICE, build_wake_message, event_body, rendered_length,
};
pub use crate::monitor_overflow::MonitorSpillResult;
use crate::monitor_overflow::{EventKind, MonitorSpill, OverflowBook};
use crate::monitor_registry::MonitorEvent;
use crate::monitor_wake_fit::fit_wake;
use crate::monitor_wake_pacing::WakePacer;
use crate::monitor_wake_undo::{RemovedSlice, WakeUndo, WakeUndoTarget};
use std::collections::BTreeSet;
pub const MONITOR_NOTIFICATION_CUSTOM_TYPE: &str = "senpi-monitor:notification";
const QUEUE_OVERHEAD_CHARS: usize = 512;
/// How many times one wake is handed to the host (admission refusals and settled failures both
/// count) before its lines are dropped with one terminal diagnostic instead of requeued again.
const MONITOR_DELIVERY_ATTEMPTS: u32 = 3;
/// Wake rate (and burst) of a paced monitor; `None` leaves the monitor unpaced.
pub type WakesPerHour = Box<dyn Fn(&str) -> Option<f64> + Send>;
fn identity(event: &MonitorEvent) -> (&str, &str, &str) {
    match event {
        MonitorEvent::Line {
            id, description, ..
        } => (id, description, "line"),
        MonitorEvent::Summary {
            id, description, ..
        } => (id, description, "summary"),
    }
}
pub fn resolve_delivery_settings(
    settings: &crate::settings::MonitorDeliverySettings,
) -> crate::settings::MonitorDeliverySettings {
    let bound = |value: f64, fallback: f64, minimum: f64, maximum: f64| {
        if value.is_finite() {
            value.trunc().clamp(minimum, maximum)
        } else {
            fallback
        }
    };
    crate::settings::MonitorDeliverySettings {
        coalesce_window_ms: bound(settings.coalesce_window_ms, 2000.0, 1.0, 60_000.0),
        rate_limit_ms: bound(settings.rate_limit_ms, 5000.0, 1.0, 3_600_000.0),
        max_lines_per_injection: bound(settings.max_lines_per_injection, 50.0, 1.0, 200.0),
        max_chars_per_injection: bound(settings.max_chars_per_injection, 4096.0, 512.0, 16_384.0),
        wake_budget: bound(settings.wake_budget, 5.0, 1.0, 100.0),
        wakes_per_hour: bound(settings.wakes_per_hour, 12.0, 1.0, 3600.0),
    }
}
pub struct MonitorNotifier {
    sender: tokio::sync::mpsc::UnboundedSender<NotifierAction>,
    task: tokio::task::JoinHandle<()>,
}
enum NotifierAction {
    Event(MonitorEvent),
    Activity,
    Resume(Vec<String>),
    /// What the delivery receipt of wake `wake` settled to, pushed back by its receipt waiter.
    Settled {
        wake: u64,
        outcome: crate::notify::DeliveryOutcome,
    },
}
/// Hands one wake to the host. `Err` is a synchronous refusal (admission failed); `Ok` carries the
/// receipt to wait on, if any, and the ids whose wake-budget pause the commit hook applies.
pub type MonitorSend = std::sync::Arc<dyn Fn(MonitorInjection) -> MonitorSendResult + Send + Sync>;
/// Applied by the loop once a wake is committed (delivered, unconfirmed, or admitted without a
/// receipt), with that wake's pause ids; never for a wake that was refused or failed.
pub type MonitorCommit = std::sync::Arc<dyn Fn(&[String]) + Send + Sync>;
/// What the host said when it was handed a wake.
pub struct MonitorAdmission {
    /// Settles once the message is appended or dropped; `None` commits the wake immediately.
    pub receipt: Option<maho_ext_api::MessageDeliveryReceipt>,
    pub pause_ids: Vec<String>,
}
impl MonitorNotifier {
    /// `send` returns what the host said about the injection; a refusal is rolled back into the
    /// queue instead of being reported on stderr and forgotten.
    pub fn new(
        settings: crate::settings::MonitorDeliverySettings,
        send: impl Fn(MonitorInjection) -> MonitorSendResult + Send + Sync + 'static,
    ) -> Self {
        Self::with_delivery_guard(settings, || true, send)
    }
    pub fn with_delivery_guard(
        settings: crate::settings::MonitorDeliverySettings,
        available: impl Fn() -> bool + Send + 'static,
        send: impl Fn(MonitorInjection) -> MonitorSendResult + Send + Sync + 'static,
    ) -> Self {
        Self::with_pacing(
            settings,
            available,
            None,
            None,
            std::sync::Arc::new(send),
            None,
        )
    }
    /// `wakes_per_hour` and `spill_overflow` are upstream `MonitorNotifierDeps.wakesPerHour` / `spillOverflow`.
    pub fn with_pacing(
        settings: crate::settings::MonitorDeliverySettings,
        available: impl Fn() -> bool + Send + 'static,
        wakes_per_hour: Option<WakesPerHour>,
        spill_overflow: Option<MonitorSpill>,
        send: MonitorSend,
        commit: Option<MonitorCommit>,
    ) -> Self {
        let (sender, task) = spawn_notifier_loop(
            settings,
            available,
            wakes_per_hour,
            spill_overflow,
            send,
            commit,
        );
        Self { sender, task }
    }
    pub fn notify_event(&self, event: MonitorEvent) -> Result<(), String> {
        self.sender
            .send(NotifierAction::Event(event))
            .map_err(|error| error.to_string())
    }
    pub fn note_activity(&self) -> Result<(), String> {
        self.sender
            .send(NotifierAction::Activity)
            .map_err(|error| error.to_string())
    }
    pub fn resume(&self, ids: Vec<String>) -> Result<(), String> {
        self.sender
            .send(NotifierAction::Resume(ids))
            .map_err(|error| error.to_string())
    }
}
/// Hands one wake to the host and settles the queue on its answer: a refusal puts the wake back
/// (its lines, its overflow and its pacing token) so the next pass retries it, and a success keeps
/// the dequeue. Either way the outcome reaches the diagnostics, never just the void.
///
/// An admission with a receipt holds the wake in the queue's single [PendingCommit] slot and
/// registers a waiter that pushes [NotifierAction::Settled] back through `settled`; the commit hook
/// runs only once the wake is committed. Returns the id of the wake it took, if any.
///
/// `report` receives every diagnostic line (`maho_ext_api::diagnostic` in production).
fn deliver(
    queue: &mut MonitorDeliveryQueue,
    now: f64,
    settings: &crate::settings::MonitorDeliverySettings,
    send: &MonitorSend,
    commit: Option<&MonitorCommit>,
    settled: &tokio::sync::mpsc::WeakUnboundedSender<NotifierAction>,
    report: &dyn Fn(&str),
) -> Option<u64> {
    if queue.awaiting_receipt() {
        return None;
    }
    let (injection, checkpoint, undo) = queue.flush_with_checkpoint(now, settings)?;
    let wake = queue.allocate_wake();
    let offered_pause_ids = injection.pause_ids.clone();
    let monitor_ids = wake_monitor_ids(&injection.details);
    match (**send)(injection) {
        Ok(MonitorAdmission {
            receipt: None,
            pause_ids,
        }) => {
            queue.retry_attempts = 0;
            apply_commit(commit, &pause_ids);
        }
        Ok(MonitorAdmission {
            receipt: Some(receipt),
            pause_ids,
        }) => {
            queue.hold_commit(PendingCommit {
                wake,
                undo,
                pause_ids,
                monitor_ids,
            });
            let settled = settled.clone();
            crate::notify::spawn_receipt_wait(
                receipt,
                move |outcome| {
                    let reported = settled.upgrade().is_some_and(|sender| {
                        sender
                            .send(NotifierAction::Settled { wake, outcome })
                            .is_ok()
                    });
                    if !reported {
                        maho_ext_api::diagnostic(&format!(
                            "monitor wake {wake} settled after its notifier stopped; outcome ignored"
                        ));
                    }
                },
                crate::notify::RECEIPT_ABANDON_AFTER,
            );
        }
        Err(error) => {
            // No interleaving window: nothing reached the queue since the take, so the
            // whole-snapshot rollback is exact here.
            queue.rollback(checkpoint);
            if queue.attempt_failed() {
                give_up(queue, &monitor_ids, &offered_pause_ids, &error, report);
            } else {
                queue.defer_retry(now + settings.rate_limit_ms);
                report(&format!(
                    "monitor wake not delivered, requeued for the next pass: {error}"
                ));
                report_pause_not_applied(&offered_pause_ids, report);
            }
        }
    }
    Some(wake)
}
fn report_pause_not_applied(pause_ids: &[String], report: &dyn Fn(&str)) {
    if !pause_ids.is_empty() {
        report(&format!(
            "monitor wake budget pause not applied to {}: the wake was not delivered",
            pause_ids.join(", ")
        ));
    }
}
/// The last allowed attempt failed (the caller already put the take back): the wake is NOT
/// requeued. Every pending line of its monitors - inline and overflow, including lines queued
/// while the attempt was outstanding - is dropped, and exactly one terminal diagnostic says so.
fn give_up(
    queue: &mut MonitorDeliveryQueue,
    monitor_ids: &[String],
    pause_ids: &[String],
    reason: &str,
    report: &dyn Fn(&str),
) {
    queue.retry_attempts = 0;
    queue.drop_lines(monitor_ids);
    let pause = if pause_ids.is_empty() {
        String::new()
    } else {
        format!(
            "; wake budget pause not applied to {}",
            pause_ids.join(", ")
        )
    };
    report(&format!(
        "monitor wake for {} dropped after {MONITOR_DELIVERY_ATTEMPTS} delivery attempts: {reason}{pause}",
        monitor_ids.join(", ")
    ));
}
/// Settles the outstanding wake `wake` on its receipt outcome and returns whether it cleared the
/// slot. A settle for any other wake (stale, or arriving after `dispose` cleared the slot) is
/// ignored with a diagnostic, never undone.
///
/// `Failed` undoes exactly what the wake took ([MonitorDeliveryQueue::undo_wake]) and defers the
/// retry; on the last allowed attempt it drops the wake instead. Neither runs the commit hook.
fn settle(
    queue: &mut MonitorDeliveryQueue,
    wake: u64,
    outcome: crate::notify::DeliveryOutcome,
    now: f64,
    settings: &crate::settings::MonitorDeliverySettings,
    commit: Option<&MonitorCommit>,
    report: &dyn Fn(&str),
) -> bool {
    let Some(pending) = queue.take_commit(wake) else {
        report(&format!(
            "monitor wake {wake} settled after it was discarded or superseded; ignored"
        ));
        return false;
    };
    match outcome {
        crate::notify::DeliveryOutcome::Delivered => {
            queue.retry_attempts = 0;
            apply_commit(commit, &pending.pause_ids);
        }
        // The message may still land: retrying would inject the same wake twice.
        crate::notify::DeliveryOutcome::Indeterminate => {
            queue.retry_attempts = 0;
            apply_commit(commit, &pending.pause_ids);
            report(&format!(
                "monitor wake for {} unconfirmed after {}s: kept as delivered and not retried, since it may still land",
                pending.monitor_ids.join(", "),
                crate::notify::RECEIPT_ABANDON_AFTER.as_secs()
            ));
        }
        crate::notify::DeliveryOutcome::Failed(reason) => {
            queue.undo_wake(pending.undo);
            if queue.attempt_failed() {
                give_up(
                    queue,
                    &pending.monitor_ids,
                    &pending.pause_ids,
                    &reason,
                    report,
                );
            } else {
                queue.defer_retry(now + settings.rate_limit_ms);
                report(&format!(
                    "monitor wake for {} failed after admission: {reason}",
                    pending.monitor_ids.join(", ")
                ));
                report_pause_not_applied(&pending.pause_ids, report);
            }
        }
    }
    true
}
/// Runs the commit hook for a committed wake (also with no pause ids: the call marks the commit).
fn apply_commit(commit: Option<&MonitorCommit>, pause_ids: &[String]) {
    if let Some(commit) = commit {
        commit(pause_ids);
    }
}
fn wake_monitor_ids(details: &serde_json::Value) -> Vec<String> {
    details["monitors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|monitor| monitor["id"].as_str().map(str::to_owned))
        .collect()
}
fn spawn_notifier_loop(
    settings: crate::settings::MonitorDeliverySettings,
    available: impl Fn() -> bool + Send + 'static,
    wakes_per_hour: Option<WakesPerHour>,
    spill_overflow: Option<MonitorSpill>,
    send: MonitorSend,
    commit: Option<MonitorCommit>,
) -> (
    tokio::sync::mpsc::UnboundedSender<NotifierAction>,
    tokio::task::JoinHandle<()>,
) {
    let settings = resolve_delivery_settings(&settings);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    // Weak: an outstanding receipt waiter must not keep the channel open, or dropping the
    // notifier's sender would never end the loop.
    let settled = sender.downgrade();
    let task = tokio::spawn(async move {
        let mut queue = MonitorDeliveryQueue::with_pacing(wakes_per_hour, spill_overflow);
        let mut deadline = None;
        let epoch = tokio::time::Instant::now();
        let after = |delay: f64| {
            tokio::time::Instant::now() + std::time::Duration::from_secs_f64(delay / 1000.0)
        };
        loop {
            let action = if let Some(due) = deadline {
                tokio::select! {action=receiver.recv()=>action,_=tokio::time::sleep_until(due)=>{
                    if !available() {queue.dispose();deadline=None;continue;}
                    let now=epoch.elapsed().as_secs_f64()*1000.0;
                    deliver(&mut queue,now,&settings,&send,commit.as_ref(),&settled,&maho_ext_api::diagnostic);
                    // A wake waiting on its receipt holds the next flush until it settles.
                    deadline=if queue.awaiting_receipt() {None} else {queue.next_delay(now,&settings).map(after)};
                    continue;
                }}
            } else {
                receiver.recv().await
            };
            match action {
                Some(NotifierAction::Event(event)) => {
                    if !available() {
                        continue;
                    }
                    let now = epoch.elapsed().as_secs_f64() * 1000.0;
                    let delay = queue.notify(event, now, &settings);
                    // Queued either way; while a receipt is outstanding the Settled arm schedules it.
                    if !queue.awaiting_receipt() {
                        let due = after(delay);
                        deadline =
                            Some(deadline.map_or(due, |old: tokio::time::Instant| old.min(due)));
                    }
                }
                Some(NotifierAction::Activity) => queue.note_activity(),
                Some(NotifierAction::Resume(ids)) => queue.resume(&ids),
                Some(NotifierAction::Settled { wake, outcome }) => {
                    let now = epoch.elapsed().as_secs_f64() * 1000.0;
                    // Only a settle that freed the slot reschedules; a stale one leaves the
                    // current deadline (and its coalesce window) alone.
                    if settle(
                        &mut queue,
                        wake,
                        outcome,
                        now,
                        &settings,
                        commit.as_ref(),
                        &maho_ext_api::diagnostic,
                    ) {
                        deadline = queue.next_delay(now, &settings).map(after);
                    }
                }
                None => return,
            }
        }
    });
    (sender, task)
}
impl Drop for MonitorNotifier {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub struct MonitorDeliveryQueue {
    events: Vec<MonitorEvent>,
    overflow: OverflowBook,
    pacer: WakePacer,
    rate_of: Option<WakesPerHour>,
    last_injection: std::collections::BTreeMap<String, f64>,
    last_batch: std::collections::BTreeMap<String, String>,
    consecutive_wakes: usize,
    last_wake: Option<f64>,
    /// Set after a refused delivery so the requeued wake is retried on the rate-limit cadence
    /// instead of spinning against a host that keeps refusing it.
    retry_not_before: Option<f64>,
    /// Id of the next wake taken by `deliver`; monotonic and never reset, so a settle for an old
    /// wake can never match a newer one (monitor ids repeat across wakes, wake ids do not).
    next_wake_id: u64,
    /// The single wake whose receipt is outstanding. See [PendingCommit] for the ordering rules.
    pending_commit: Option<PendingCommit>,
    /// Failed attempts of the wake being retried (admission refusals and settled failures);
    /// reset by every commit, by a give-up, and by `dispose`. See [MONITOR_DELIVERY_ATTEMPTS].
    retry_attempts: u32,
    /// Bumped by every `note_activity` (and so every `resume`): a wake undo rewinds the wake
    /// streak only if no activity was noted since the wake was taken.
    activity: u64,
}
/// A wake handed to the host whose delivery receipt has not settled yet.
///
/// ORDERING RULES while this slot is occupied:
/// 1. The loop takes no other wake: its flush deadline stays `None` (and `deliver` refuses) until
///    this wake settles, so at most one wake is ever in flight.
/// 2. `Event`, `Activity` and `Resume` keep draining: new monitor output is queued, not lost, but
///    the `Event` arm sets no deadline, or lines queued now would sit until the next event.
/// 3. Every `Settled` arm that clears the slot recomputes the deadline from `next_delay`.
///
/// `dispose` clears the slot; a settle arriving afterwards matches nothing and is ignored with a
/// diagnostic, so it can never resurrect lines into a disposed queue.
struct PendingCommit {
    wake: u64,
    /// What the wake took, recorded at take time, so a `Failed` settle can put back exactly that.
    undo: WakeUndo,
    pause_ids: Vec<String>,
    monitor_ids: Vec<String>,
}
/// The queue exactly as it stood before a wake was taken out of it. Taking a wake dequeues its
/// lines, forgets its overflow and spends its pacing token; if the host then refuses the message
/// none of that may stand, or the wake is lost with no trace. Restoring this puts every line back
/// for the next attempt.
#[derive(Clone)]
pub struct MonitorCheckpoint {
    events: Vec<MonitorEvent>,
    overflow: indexmap::IndexMap<String, crate::monitor_overflow::MonitorOverflow>,
    pacer: WakePacer,
    last_injection: std::collections::BTreeMap<String, f64>,
    last_batch: std::collections::BTreeMap<String, String>,
    consecutive_wakes: usize,
    last_wake: Option<f64>,
}
/// What the host reports for one wake it was handed: `Err` is a synchronous refusal.
pub type MonitorSendResult = Result<MonitorAdmission, String>;
impl Default for MonitorDeliveryQueue {
    fn default() -> Self {
        Self::with_pacing(None, None)
    }
}
pub struct MonitorInjection {
    pub content: String,
    pub details: serde_json::Value,
    pub pause_ids: Vec<String>,
}
/// Per-monitor attribution of one wake: what it carried inline plus what overflowed.
pub fn monitor_details(
    events: &[MonitorEvent],
    ids: &[String],
    book: &OverflowBook,
) -> Vec<serde_json::Value> {
    ids.iter().map(|id| {
        let own=events.iter().filter(|event|identity(event).0==id).collect::<Vec<_>>();let overflow=book.get(id);
        let mut kinds:Vec<EventKind>=vec![];
        for kind in own.iter().map(|event|EventKind::of(event)).chain(overflow.into_iter().flat_map(|entry|entry.kinds.iter().copied())) {if !kinds.contains(&kind) {kinds.push(kind);}}
        let human_input=own.iter().any(|event|matches!(event,MonitorEvent::Line {human_input:true,..}))||overflow.is_some_and(|entry|entry.human_input);
        let mut detail=serde_json::json!({"id":id,"description":own.first().map(|event|identity(event).1).or_else(||overflow.map(|entry|entry.description.as_str())).unwrap_or(id),"eventCount":own.len()+overflow.map_or(0,|entry|entry.count),"kinds":kinds.iter().map(|kind|kind.as_str()).collect::<Vec<_>>()});
        if human_input {detail["humanInput"]=serde_json::json!(true);}
        detail
    }).collect()
}
impl MonitorDeliveryQueue {
    pub fn with_pacing(rate_of: Option<WakesPerHour>, spill: Option<MonitorSpill>) -> Self {
        Self {
            events: vec![],
            overflow: OverflowBook::new(spill),
            pacer: WakePacer::new(),
            rate_of,
            last_injection: Default::default(),
            last_batch: Default::default(),
            consecutive_wakes: 0,
            last_wake: None,
            retry_not_before: None,
            next_wake_id: 0,
            pending_commit: None,
            retry_attempts: 0,
            activity: 0,
        }
    }
    pub fn dispose(&mut self) {
        self.events.clear();
        self.overflow.clear();
        self.last_batch.clear();
        self.pacer.clear();
        self.retry_not_before = None;
        self.pending_commit = None;
        self.retry_attempts = 0;
    }
    /// Counts one failed attempt; true when it was the last allowed one, so the wake is dropped.
    fn attempt_failed(&mut self) -> bool {
        self.retry_attempts += 1;
        self.retry_attempts >= MONITOR_DELIVERY_ATTEMPTS
    }
    /// Drops every pending line (queued and overflowed) of `ids`.
    fn drop_lines(&mut self, ids: &[String]) {
        self.events
            .retain(|event| !ids.iter().any(|id| id == identity(event).0));
        for id in ids {
            self.overflow.delete(id);
        }
    }
    /// Puts back exactly what one wake took (see `monitor_wake_undo`), leaving every line, overflow
    /// count and stamp that arrived since untouched.
    fn undo_wake(&mut self, undo: WakeUndo) {
        undo.apply(WakeUndoTarget {
            events: &mut self.events,
            overflow: &mut self.overflow,
            pacer: &mut self.pacer,
            last_injection: &mut self.last_injection,
            last_batch: &mut self.last_batch,
            consecutive_wakes: &mut self.consecutive_wakes,
            last_wake: &mut self.last_wake,
            activity: self.activity,
        });
    }
    fn allocate_wake(&mut self) -> u64 {
        let wake = self.next_wake_id;
        self.next_wake_id += 1;
        wake
    }
    fn awaiting_receipt(&self) -> bool {
        self.pending_commit.is_some()
    }
    fn hold_commit(&mut self, pending: PendingCommit) {
        self.pending_commit = Some(pending);
    }
    /// Takes the outstanding wake if it is `wake`; any other id leaves the slot untouched.
    fn take_commit(&mut self, wake: u64) -> Option<PendingCommit> {
        if self
            .pending_commit
            .as_ref()
            .is_some_and(|pending| pending.wake == wake)
        {
            self.pending_commit.take()
        } else {
            None
        }
    }
    fn ready_at(&mut self, id: &str, now: f64) -> f64 {
        let rate = self.rate_of.as_ref().and_then(|rate_of| rate_of(id));
        self.pacer.ready_at(id, rate, now)
    }
    /// Queues `event` and returns the delay in ms until the next flush should run.
    pub fn notify(
        &mut self,
        event: MonitorEvent,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> f64 {
        let (id, summary) = {
            let (id, _, kind) = identity(&event);
            (id.to_owned(), kind == "summary")
        };
        if summary {
            self.last_injection.remove(&id);
            self.last_batch.remove(&id);
        }
        let queue_limit = (settings.max_chars_per_injection as usize)
            .saturating_sub(QUEUE_OVERHEAD_CHARS)
            .max(1);
        let own = self
            .events
            .iter()
            .filter(|queued| identity(queued).0 == id)
            .collect::<Vec<_>>();
        let own_chars = own
            .iter()
            .map(|queued| rendered_length(queued))
            .sum::<usize>();
        if !summary
            && (own.len() >= settings.max_lines_per_injection as usize
                || own_chars + rendered_length(&event) > queue_limit
                || self.overflow.count(&id) > 0)
        {
            let body = event_body(&event);
            let human_input = matches!(
                &event,
                MonitorEvent::Line {
                    human_input: true,
                    ..
                }
            );
            self.overflow.record(&event, &body, human_input);
        } else {
            self.events.push(event);
        }
        // A paced monitor out of tokens is not flushed before its next token: no wasted wake-ups.
        if summary {
            settings.coalesce_window_ms
        } else {
            settings
                .coalesce_window_ms
                .max(self.ready_at(&id, now) - now)
        }
    }
    pub fn note_activity(&mut self) {
        self.consecutive_wakes = 0;
        self.activity = self.activity.wrapping_add(1);
    }
    pub fn resume(&mut self, ids: &[String]) {
        for id in ids {
            self.last_injection.remove(id);
            self.last_batch.remove(id);
        }
        self.note_activity();
    }
    fn pending_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = vec![];
        for id in self
            .events
            .iter()
            .map(|event| identity(event).0.to_owned())
            .chain(self.overflow.ids())
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }
    /// Delay in ms until the earliest pending monitor may wake again; `None` when nothing is pending.
    pub fn next_delay(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<f64> {
        let ids = self.pending_ids();
        let summary_ids = self.summary_ids();
        let earliest = ids
            .iter()
            .map(|id| {
                // Same exemption as take_ready_wake: a summary-bearing id skips both the rate-limit
                // stamp and the pacer, so it is due now (the floor below keeps this off zero).
                if summary_ids.contains(id) {
                    return 0.0;
                }
                let ready = self.ready_at(id, now);
                // An id with no stamp still waits rate_limit_ms on purpose: these are ids fit_wake
                // left out of a wake for character budget, and waking them 1 ms later would send
                // back-to-back wakes and burn the wake budget.
                (self.last_injection.get(id).copied().unwrap_or(now) + settings.rate_limit_ms)
                    .max(ready)
                    - now
            })
            .min_by(f64::total_cmp);
        match (earliest, self.retry_not_before) {
            (Some(delay), Some(at)) => Some(delay.max(at - now).max(1.0)),
            (Some(delay), None) => Some(delay.max(1.0)),
            (None, Some(at)) => Some((at - now).max(1.0)),
            (None, None) => None,
        }
    }
    /// Pending ids holding any `Summary` event: exempt from both the rate-limit stamp and the pacer.
    /// Shared by `next_delay` and `take_ready_wake` so their eligibility cannot drift.
    fn summary_ids(&self) -> BTreeSet<String> {
        self.events
            .iter()
            .filter(|event| identity(event).2 == "summary")
            .map(|event| identity(event).0.to_owned())
            .collect()
    }
    pub fn flush(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<MonitorInjection> {
        self.take_wake(now, settings)
            .map(|(injection, _)| injection)
    }

    /// Takes the next wake out of the queue together with both ways to put it back: the
    /// whole-snapshot [MonitorCheckpoint] for a synchronous admission refusal ([Self::rollback]),
    /// and the [WakeUndo] for a receipt that settles `Failed` later, after other lines may have
    /// arrived ([Self::undo_wake]).
    pub(crate) fn flush_with_checkpoint(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<(MonitorInjection, MonitorCheckpoint, WakeUndo)> {
        let checkpoint = self.checkpoint();
        let (injection, undo) = self.take_wake(now, settings)?;
        Some((injection, checkpoint, undo))
    }

    fn checkpoint(&self) -> MonitorCheckpoint {
        MonitorCheckpoint {
            events: self.events.clone(),
            overflow: self.overflow.snapshot(),
            pacer: self.pacer.clone(),
            last_injection: self.last_injection.clone(),
            last_batch: self.last_batch.clone(),
            consecutive_wakes: self.consecutive_wakes,
            last_wake: self.last_wake,
        }
    }

    /// Undoes one [Self::flush_with_checkpoint]: the wake's lines are queued again and its pacing
    /// token and rate-limit stamp are given back, so the next pass retries the same wake.
    ///
    /// DIVERGENCE: overflow lines already spilled to their file by the aborted attempt are written
    /// again by the retry, so that diagnostic file can hold the same chunk twice. Losing the wake
    /// is worse than a duplicated spill line.
    pub fn rollback(&mut self, checkpoint: MonitorCheckpoint) {
        self.events = checkpoint.events;
        self.overflow.restore(checkpoint.overflow);
        self.pacer = checkpoint.pacer;
        self.last_injection = checkpoint.last_injection;
        self.last_batch = checkpoint.last_batch;
        self.consecutive_wakes = checkpoint.consecutive_wakes;
        self.last_wake = checkpoint.last_wake;
    }

    /// Holds the next attempt until `at` (ms): a requeued wake must not retry in a tight loop.
    pub fn defer_retry(&mut self, at: f64) {
        self.retry_not_before = Some(at);
    }

    fn take_wake(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<(MonitorInjection, WakeUndo)> {
        if self.retry_not_before.is_some_and(|at| now < at) {
            return None;
        }
        // The gate is spent the moment it elapses, even with nothing left to send: keeping it would
        // make next_delay report a delay forever and spin the notifier on an empty queue.
        self.retry_not_before = None;
        self.take_ready_wake(now, settings)
    }

    /// Takes the next ready wake and returns it with the [WakeUndo] recording exactly what the take
    /// removed and spent, captured as it happens (the removed lines are only known in here).
    fn take_ready_wake(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<(MonitorInjection, WakeUndo)> {
        let pending = self.pending_ids();
        if pending.is_empty() {
            return None;
        }
        // Pre-wake state for the undo: the streak, and each monitor's first queue position.
        let consecutive_before = self.consecutive_wakes;
        let last_wake_before = self.last_wake;
        let mut ordinals = std::collections::BTreeMap::<String, usize>::new();
        for (index, event) in self.events.iter().enumerate() {
            ordinals
                .entry(identity(event).0.to_owned())
                .or_insert(index);
        }
        let ordinal = |id: &str| ordinals.get(id).copied().unwrap_or(0);
        let summary_ids = self.summary_ids();
        let mut ready = vec![];
        for id in pending {
            if summary_ids.contains(&id) {
                ready.push(id);
                continue;
            }
            if self
                .last_injection
                .get(&id)
                .is_some_and(|last| now - last < settings.rate_limit_ms)
            {
                continue;
            }
            if self.ready_at(&id, now) <= now {
                ready.push(id);
            }
        }
        if ready.is_empty() {
            return None;
        }
        let mut fingerprints = std::collections::BTreeMap::new();
        for id in &ready {
            if self.overflow.count(id) > 0 {
                continue;
            }
            let batch = self
                .events
                .iter()
                .filter(|event| identity(event).0 == id)
                .collect::<Vec<_>>();
            if batch.is_empty() || batch.iter().any(|event| identity(event).2 != "line") {
                continue;
            }
            fingerprints.insert(
                id.clone(),
                batch
                    .into_iter()
                    .map(event_body)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        let suppressed = fingerprints
            .iter()
            .filter(|(id, fingerprint)| self.last_batch.get(*id) == Some(*fingerprint))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let suppressed_slices = suppressed
            .iter()
            .map(|id| RemovedSlice {
                id: id.clone(),
                ordinal: ordinal(id.as_str()),
                events: self
                    .events
                    .iter()
                    .filter(|event| identity(event).0 == id)
                    .cloned()
                    .collect(),
            })
            .collect::<Vec<_>>();
        self.events
            .retain(|event| !suppressed.iter().any(|id| id == identity(event).0));
        let candidates = ready
            .into_iter()
            .filter(|id| !suppressed.contains(id))
            .collect::<Vec<_>>();
        let wake = fit_wake(
            &self.events,
            &mut self.overflow,
            &candidates,
            &summary_ids,
            settings.max_chars_per_injection as usize,
        );
        if wake.is_empty() {
            return None;
        }
        let injected = wake.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        let selected = wake
            .iter()
            .flat_map(|(_, share)| share.events.iter().cloned())
            .collect::<Vec<_>>();
        if self
            .last_wake
            .is_some_and(|last| now - last > settings.rate_limit_ms * 2.0)
        {
            self.consecutive_wakes = 0;
        }
        // A runaway-guard pause summary is a pause, not a completion: exempting it keeps the
        // session-global wake streak (the notifier's own pause state) from being cleared.
        let completion=selected.iter().any(|event|matches!(event,MonitorEvent::Summary {summary,..} if !summary.starts_with(crate::shared::RUNAWAY_PAUSE_SUMMARY_PREFIX)));
        let reaches_budget =
            !completion && self.consecutive_wakes + 1 >= settings.wake_budget as usize;
        let notices = wake
            .iter()
            .map(|(id, share)| {
                if share.whole {
                    self.overflow.notice(id)
                } else {
                    String::new()
                }
            })
            .collect::<Vec<_>>();
        let content = build_wake_message(
            &selected,
            &notices,
            if reaches_budget {
                WAKE_BUDGET_PAUSE_NOTICE
            } else {
                ""
            },
            settings.max_chars_per_injection as usize,
        );
        let details =
            serde_json::json!({"monitors":monitor_details(&selected,&injected,&self.overflow)});
        self.last_wake = Some(now);
        let mut taken = vec![];
        let mut deleted_overflow = vec![];
        let mut retained_overflow = vec![];
        let mut spent = vec![];
        let mut last_injection_before = vec![];
        let mut last_batch_before = vec![];
        for (id, share) in &wake {
            let rate = self.rate_of.as_ref().and_then(|rate_of| rate_of(id));
            if share
                .events
                .iter()
                .any(|event| identity(event).2 == "summary")
            {
                self.pacer.forget(id);
            } else {
                self.pacer.spend(id, rate, now);
                spent.push((id.clone(), rate));
            }
            last_injection_before.push((id.clone(), self.last_injection.insert(id.clone(), now)));
            // A monitor that sent only a prefix keeps its remaining lines and overflow for the next wake.
            if share.whole {
                if let Some(entry) = self.overflow.remove_full(id) {
                    deleted_overflow.push(entry);
                }
            } else if self.overflow.get(id).is_some() {
                retained_overflow.push(id.clone());
            }
            let written = if share.whole {
                fingerprints.remove(id)
            } else {
                None
            };
            let before = match &written {
                Some(batch) => self.last_batch.insert(id.clone(), batch.clone()),
                None => self.last_batch.remove(id),
            };
            last_batch_before.push((id.clone(), before, written));
            if !share.events.is_empty() {
                taken.push(RemovedSlice {
                    id: id.clone(),
                    ordinal: ordinal(id.as_str()),
                    events: share.events.clone(),
                });
            }
            let mut remaining = share.events.len();
            self.events.retain(|event| {
                if remaining > 0 && identity(event).0 == id {
                    remaining -= 1;
                    false
                } else {
                    true
                }
            });
        }
        self.consecutive_wakes = if completion {
            0
        } else {
            self.consecutive_wakes + 1
        };
        // Only the monitors in this wake are paused; every other monitor keeps its held lines.
        if reaches_budget {
            self.consecutive_wakes = 0;
        }
        let undo = WakeUndo {
            taken,
            suppressed: suppressed_slices,
            deleted_overflow,
            retained_overflow,
            spent,
            stamped_at: now,
            last_injection: last_injection_before,
            last_batch: last_batch_before,
            consecutive_wakes: consecutive_before,
            last_wake: last_wake_before,
            activity: self.activity,
        };
        Some((
            MonitorInjection {
                content,
                details,
                pause_ids: if reaches_budget { injected } else { vec![] },
            },
            undo,
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn delivery_disabled_before_flush_discards_queue_and_recovers() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let available = Arc::new(AtomicBool::new(true));
        let guard = available.clone();
        let (checks, mut checked) = tokio::sync::mpsc::unbounded_channel();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let notifier = MonitorNotifier::with_delivery_guard(
            crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor,
            move || {
                let value = guard.load(Ordering::SeqCst);
                checks.send(value).unwrap();
                value
            },
            move |injection| admit(&sender, injection),
        );
        notifier.notify_event(line("discarded")).unwrap();
        assert_eq!(checked.recv().await, Some(true));
        available.store(false, Ordering::SeqCst);
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(checked.recv().await, Some(false));
        assert!(receiver.try_recv().is_err());
        available.store(true, Ordering::SeqCst);
        notifier.notify_event(line("recovered")).unwrap();
        assert_eq!(checked.recv().await, Some(true));
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        let injection = receiver.recv().await.unwrap();
        assert!(injection.content.contains("recovered"));
        assert!(!injection.content.contains("discarded"));
        drop(notifier);
    }
    #[tokio::test]
    async fn scheduled_delivery_wakes_through_exact_injection_signal() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.coalesce_window_ms = 1.0;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let notifier = MonitorNotifier::new(settings, move |injection| admit(&sender, injection));
        notifier.notify_event(line("ready")).unwrap();
        let injection = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(injection.content.contains("Monitor event(watch): ready"));
        assert_eq!(injection.details["monitors"][0]["eventCount"], 1);
    }
    /// Forwards the injection and admits it with no receipt: the wake commits immediately.
    fn admit(
        sender: &tokio::sync::mpsc::UnboundedSender<MonitorInjection>,
        injection: MonitorInjection,
    ) -> MonitorSendResult {
        let pause_ids = injection.pause_ids.clone();
        sender.send(injection).map_err(|error| error.to_string())?;
        Ok(MonitorAdmission {
            receipt: None,
            pause_ids,
        })
    }
    fn line_on(id: &str, text: &str) -> MonitorEvent {
        MonitorEvent::Line {
            id: id.to_owned(),
            description: "watch".to_owned(),
            line: text.to_owned(),
            human_input: false,
        }
    }
    type Injections = tokio::sync::mpsc::UnboundedReceiver<MonitorInjection>;
    type Receipts = tokio::sync::mpsc::UnboundedReceiver<maho_ext_api::MessageDeliveryReceipt>;
    type Commits = tokio::sync::mpsc::UnboundedReceiver<Vec<String>>;
    /// A send that forwards each injection and admits it with a fresh pending receipt the test
    /// settles, plus a commit hook that forwards each commit.
    fn receipt_harness() -> (MonitorSend, MonitorCommit, Injections, Receipts, Commits) {
        let (injected, injections) = tokio::sync::mpsc::unbounded_channel();
        let (issued, receipts) = tokio::sync::mpsc::unbounded_channel();
        let (committed, commits) = tokio::sync::mpsc::unbounded_channel();
        let send: MonitorSend =
            std::sync::Arc::new(move |injection: MonitorInjection| -> MonitorSendResult {
                let pause_ids = injection.pause_ids.clone();
                injected
                    .send(injection)
                    .map_err(|error| error.to_string())?;
                let receipt = maho_ext_api::MessageDeliveryReceipt::pending();
                issued
                    .send(receipt.clone())
                    .map_err(|error| error.to_string())?;
                Ok(MonitorAdmission {
                    receipt: Some(receipt),
                    pause_ids,
                })
            });
        let commit: MonitorCommit = std::sync::Arc::new(move |ids: &[String]| {
            committed.send(ids.to_vec()).expect("commit receiver");
        });
        (send, commit, injections, receipts, commits)
    }
    async fn next<T>(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<T>, what: &str) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(120), receiver.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out awaiting {what}"))
            .unwrap_or_else(|| panic!("{what} channel closed"))
    }
    #[tokio::test(start_paused = true)]
    async fn an_admission_without_a_receipt_commits_immediately() {
        let (injected, mut injections) = tokio::sync::mpsc::unbounded_channel();
        let (committed, mut commits) = tokio::sync::mpsc::unbounded_channel();
        let notifier = MonitorNotifier::with_pacing(
            crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor,
            || true,
            None,
            None,
            std::sync::Arc::new(move |injection: MonitorInjection| admit(&injected, injection)),
            Some(std::sync::Arc::new(move |ids: &[String]| {
                committed.send(ids.to_vec()).expect("commit receiver");
            })),
        );
        notifier.notify_event(line("landed")).unwrap();
        let injection = next(&mut injections, "injection").await;
        assert!(injection.content.contains("landed"));
        assert_eq!(next(&mut commits, "commit").await, Vec::<String>::new());
    }
    /// Each `Settled` kind is accepted and the loop goes on taking wakes; only Delivered and
    /// Indeterminate commit. A wake is never taken while a receipt is outstanding.
    #[tokio::test(start_paused = true)]
    async fn every_settled_outcome_is_accepted_and_the_loop_keeps_running() {
        let (send, commit, mut injections, mut receipts, mut commits) = receipt_harness();
        let notifier = MonitorNotifier::with_pacing(
            crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor,
            || true,
            None,
            None,
            send,
            Some(commit),
        );
        notifier.notify_event(line_on("m0", "zero")).unwrap();
        assert!(
            next(&mut injections, "wake 0")
                .await
                .content
                .contains("zero")
        );
        // Queued while wake 0 is outstanding: held, not taken, until wake 0 settles.
        notifier.notify_event(line_on("m1", "one")).unwrap();
        next(&mut receipts, "receipt 0").await.resolve_delivered();
        assert_eq!(next(&mut commits, "commit 0").await, Vec::<String>::new());
        assert!(
            next(&mut injections, "wake 1")
                .await
                .content
                .contains("one")
        );
        next(&mut receipts, "receipt 1")
            .await
            .resolve_failed("queue cleared");
        notifier.notify_event(line_on("m2", "two")).unwrap();
        assert!(
            next(&mut injections, "wake 2")
                .await
                .content
                .contains("two")
        );
        assert!(
            commits.try_recv().is_err(),
            "a failed wake is not committed"
        );
        // Never settled: the waiter reports Indeterminate at its deadline, which commits.
        let _unsettled = next(&mut receipts, "receipt 2").await;
        assert_eq!(next(&mut commits, "commit 2").await, Vec::<String>::new());
        notifier.notify_event(line_on("m3", "three")).unwrap();
        assert!(
            next(&mut injections, "wake 3")
                .await
                .content
                .contains("three")
        );
        // A settle for a wake that is not outstanding is ignored, and the loop still runs.
        for outcome in [
            crate::notify::DeliveryOutcome::Delivered,
            crate::notify::DeliveryOutcome::Failed("stale".to_owned()),
            crate::notify::DeliveryOutcome::Indeterminate,
        ] {
            notifier
                .sender
                .send(NotifierAction::Settled {
                    wake: u64::MAX,
                    outcome,
                })
                .unwrap();
        }
        next(&mut receipts, "receipt 3").await.resolve_delivered();
        assert_eq!(next(&mut commits, "commit 3").await, Vec::<String>::new());
        assert!(commits.try_recv().is_err(), "stale settles commit nothing");
    }
    /// The receipt waiter holds only a weak sender: dropping the notifier's sender ends the loop
    /// even while a wake's receipt is still outstanding.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_sender_ends_the_loop_with_a_receipt_outstanding() {
        let (send, commit, mut injections, mut receipts, _commits) = receipt_harness();
        let (sender, task) = spawn_notifier_loop(
            crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor,
            || true,
            None,
            None,
            send,
            Some(commit),
        );
        sender
            .send(NotifierAction::Event(line("in flight")))
            .unwrap();
        next(&mut injections, "injection").await;
        let _outstanding = next(&mut receipts, "receipt").await;
        drop(sender);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("the loop ends once its sender is dropped")
            .expect("the loop ends cleanly");
    }
    fn line(text: &str) -> MonitorEvent {
        MonitorEvent::Line {
            id: "b1".to_owned(),
            description: "watch".to_owned(),
            line: text.to_owned(),
            human_input: false,
        }
    }
    /// A refused delivery used to lose the wake: the flush dequeued the lines and spent the pacing
    /// token before the host could answer. The checkpoint/rollback pair must put the same wake back.
    #[test]
    fn a_refused_wake_is_rolled_back_instead_of_lost() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line("must survive"), 0.0, &settings);
        let (injection, checkpoint, _) = queue
            .flush_with_checkpoint(0.0, &settings)
            .expect("wake is ready");
        assert!(injection.content.contains("must survive"));
        assert!(
            queue.flush(0.0, &settings).is_none(),
            "pacing token was spent"
        );
        queue.rollback(checkpoint);
        let retried = queue
            .flush(0.0, &settings)
            .expect("the rolled-back wake is delivered again");
        assert!(retried.content.contains("must survive"));
    }

    /// A requeued wake is retried on the delivery cadence, not against the notifier's own clock:
    /// the loop asks `next_delay` and must be told to wait, or a refusing host spins the queue.
    #[test]
    fn a_requeued_wake_is_retried_only_after_the_rate_limit() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("must retry"), 0.0, &settings);
        let (_, checkpoint, _) = queue
            .flush_with_checkpoint(0.0, &settings)
            .expect("wake is ready");
        queue.rollback(checkpoint);
        queue.defer_retry(settings.rate_limit_ms);
        assert!(
            queue.flush(0.0, &settings).is_none(),
            "a rolled-back wake must not be retried in the same pass"
        );
        assert_eq!(
            queue.next_delay(0.0, &settings),
            Some(settings.rate_limit_ms),
            "the notifier sleep must follow the retry gate"
        );
        assert!(
            queue.flush(settings.rate_limit_ms, &settings).is_some(),
            "the retry lands once the gate elapses"
        );
    }

    /// The wake budget pauses only the monitors a wake reached; a refused wake must not pause them.
    #[test]
    fn a_refused_wake_does_not_spend_the_wake_budget_pause() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.wake_budget = 1.0;
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("one"), 0.0, &settings);
        let (_, checkpoint, _) = queue
            .flush_with_checkpoint(0.0, &settings)
            .expect("wake is ready");
        queue.rollback(checkpoint);
        assert_eq!(queue.consecutive_wakes, 0);
        let retried = queue
            .flush(0.0, &settings)
            .expect("the rolled-back wake is delivered again");
        assert_eq!(retried.pause_ids, ["b1"]);
    }

    /// Production-path regression: the refusal arrives as a SETTLE failure on an admitted wake, not
    /// as a send error. `deliver` + `settle` must undo the wake, keep its lines queued for the next
    /// pass, and leave the commit hook unrun for the wake that never landed.
    #[tokio::test(start_paused = true)]
    async fn a_failed_settle_retries_through_the_production_deliver_path() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let (send, commit, mut injections, _receipts, mut commits) = receipt_harness();
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line("must survive settle"), 0.0, &settings);
        let (_open, settled) = settle_channel();
        let report = |_: &str| {};
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &send,
            Some(&commit),
            &settled,
            &report,
        )
        .expect("a wake was due");
        assert!(
            next(&mut injections, "wake")
                .await
                .content
                .contains("must survive settle")
        );
        assert!(queue.awaiting_receipt());
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Failed("host refused".to_owned()),
            0.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert!(
            commits.try_recv().is_err(),
            "the commit hook must not run for a wake that never landed"
        );
        assert!(
            !queue.awaiting_receipt(),
            "the failed settle must clear the receipt slot"
        );
        assert!(
            queue.flush(0.0, &settings).is_none(),
            "the retry waits for the rate limit"
        );
        let retried = queue
            .flush(settings.rate_limit_ms, &settings)
            .expect("the lines survive the failed settle");
        assert!(retried.content.contains("must survive settle"));
        drop(_open);
    }

    /// Production-path regression: a failed settle returns the pacing token. The queue is NON-EMPTY
    /// here, where the older test asserted against an empty one and so proved nothing.
    #[tokio::test(start_paused = true)]
    async fn a_failed_settle_returns_the_pacing_token_with_a_non_empty_queue() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let (send, commit, mut injections, _receipts, _commits) = receipt_harness();
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line_on("b1", "first"), 0.0, &settings);
        queue.notify(line_on("b2", "second"), 0.0, &settings);
        let (_open, settled) = settle_channel();
        let report = |_: &str| {};
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &send,
            Some(&commit),
            &settled,
            &report,
        )
        .expect("a wake was due");
        assert!(
            next(&mut injections, "wake")
                .await
                .content
                .contains("first")
        );
        assert!(queue.awaiting_receipt());
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Failed("refused".to_owned()),
            0.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert!(
            !queue.events.is_empty(),
            "the queue still holds the lines the failed wake did not carry"
        );
        assert!(
            !queue.awaiting_receipt(),
            "the token came back, so deliver can take a wake again"
        );
        assert!(
            queue.events.iter().any(|event| matches!(
                event,
                MonitorEvent::Line { line, .. } if line.contains("first")
            )),
            "the failed wake's own line is queued for the retry"
        );
    }

    /// Production-path regression: at `wake_budget = 3` a restored counter and a reset counter give
    /// DIFFERENT pause decisions, so the assertion discriminates where a budget of 1 could not. A
    /// failed settle must leave the counter untouched.
    #[tokio::test(start_paused = true)]
    async fn a_failed_settle_does_not_spend_the_wake_budget_at_budget_three() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.wake_budget = 3.0;
        let (send, commit, mut injections, _receipts, mut commits) = receipt_harness();
        let mut queue = MonitorDeliveryQueue::default();
        let (_open, settled) = settle_channel();
        let report = |_: &str| {};
        queue.notify(line_on("b1", "one"), 0.0, &settings);
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &send,
            Some(&commit),
            &settled,
            &report,
        )
        .expect("first wake");
        next(&mut injections, "wake 1").await;
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Failed("refused".to_owned()),
            0.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert_eq!(
            queue.consecutive_wakes, 0,
            "the refused wake left the budget untouched"
        );
        queue.defer_retry(0.0);
        let retry_wake = deliver(
            &mut queue,
            1.0,
            &settings,
            &send,
            Some(&commit),
            &settled,
            &report,
        )
        .expect("retry wake");
        let retry = next(&mut injections, "retry wake").await;
        assert_eq!(
            retry.pause_ids,
            Vec::<String>::new(),
            "wake 1 of the budget does not pause"
        );
        assert!(settle(
            &mut queue,
            retry_wake,
            crate::notify::DeliveryOutcome::Delivered,
            1.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert_eq!(next(&mut commits, "commit 1").await, Vec::<String>::new());
        assert_eq!(queue.consecutive_wakes, 1);
        let third_at = settings.rate_limit_ms * 2.0;
        queue.notify(line_on("b1", "two"), third_at, &settings);
        let second = deliver(
            &mut queue,
            third_at,
            &settings,
            &send,
            Some(&commit),
            &settled,
            &report,
        )
        .expect("second budgeted wake");
        next(&mut injections, "wake 2").await;
        assert!(settle(
            &mut queue,
            second,
            crate::notify::DeliveryOutcome::Delivered,
            2.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert_eq!(
            queue.consecutive_wakes, 2,
            "the refusal cost no slot: this is wake 2, not wake 3"
        );
        assert_eq!(next(&mut commits, "commit 2").await, Vec::<String>::new());
    }

    /// Production-path regression: a Delivered settle runs the commit hook exactly once with the
    /// wake's own pause ids - not zero times (a lost pause) and not twice (a double charge).
    #[tokio::test(start_paused = true)]
    async fn a_delivered_settle_commits_exactly_once_with_its_pause_ids() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.wake_budget = 1.0;
        let (send, commit, mut injections, _receipts, mut commits) = receipt_harness();
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line_on("b1", "landed"), 0.0, &settings);
        let (_open, settled) = settle_channel();
        let report = |_: &str| {};
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &send,
            Some(&commit),
            &settled,
            &report,
        )
        .expect("wake");
        let injection = next(&mut injections, "wake").await;
        assert_eq!(injection.pause_ids, ["b1"]);
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Delivered,
            0.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert_eq!(next(&mut commits, "commit").await, vec!["b1".to_owned()]);
        assert!(
            commits.try_recv().is_err(),
            "the hook runs once per settled wake"
        );
        drop(_open);
    }

    /// monitor-notify-details.ts: a wake carrying a human-input line (inline or overflowed) is
    /// tagged `humanInput`; machine lines are not.
    #[test]
    fn human_input_lines_tag_the_wake_details_inline_and_through_overflow() {
        let human = |text: &str| MonitorEvent::Line {
            id: "b1".to_owned(),
            description: "watch".to_owned(),
            line: text.to_owned(),
            human_input: true,
        };
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(human("hello"), 0.0, &settings);
        let injection = queue.flush(0.0, &settings).unwrap();
        assert_eq!(injection.details["monitors"][0]["humanInput"], true);
        let mut machine = MonitorDeliveryQueue::default();
        machine.notify(line("status"), 0.0, &settings);
        let injection = machine.flush(0.0, &settings).unwrap();
        assert!(injection.details["monitors"][0].get("humanInput").is_none());
        let mut tight = settings;
        tight.max_lines_per_injection = 1.0;
        let mut overflowing = MonitorDeliveryQueue::default();
        overflowing.notify(line("first"), 0.0, &tight);
        overflowing.notify(human("second"), 0.0, &tight);
        assert!(overflowing.overflow.get("b1").unwrap().human_input);
    }
    #[test]
    fn rate_limit_and_duplicate_batches_do_not_consume_wake_budget() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("one"), 0.0, &settings);
        assert!(queue.flush(0.0, &settings).is_some());
        queue.notify(line("one"), 100.0, &settings);
        assert!(queue.flush(100.0, &settings).is_none());
        assert!(queue.flush(5000.0, &settings).is_none());
        assert_eq!(queue.consecutive_wakes, 1);
        assert!(queue.pending_ids().is_empty());
        queue.notify(line("two"), 5001.0, &settings);
        assert!(queue.flush(5001.0, &settings).is_some());
        assert_eq!(queue.consecutive_wakes, 2);
    }
    #[test]
    fn completion_bypasses_rate_limit_and_wake_budget_mutes_only_updates() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.wake_budget = 1.0;
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("one"), 0.0, &settings);
        assert_eq!(queue.flush(0.0, &settings).unwrap().pause_ids, ["b1"]);
        queue.notify(
            MonitorEvent::Summary {
                id: "b1".to_owned(),
                description: "watch".to_owned(),
                summary: "watcher completed".to_owned(),
            },
            1.0,
            &settings,
        );
        assert!(queue.flush(1.0, &settings).unwrap().pause_ids.is_empty());
    }
    #[test]
    fn runaway_pause_summary_is_not_a_completion_and_keeps_the_wake_streak() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.wake_budget = 3.0;
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("one"), 0.0, &settings);
        assert!(queue.flush(0.0, &settings).is_some());
        assert_eq!(queue.consecutive_wakes, 1);
        queue.notify(
            MonitorEvent::Summary {
                id: "b1".to_owned(),
                description: "watch".to_owned(),
                summary: crate::shared::runaway_pause_summary("b1", 120, 5),
            },
            1.0,
            &settings,
        );
        assert!(queue.flush(1.0, &settings).is_some());
        assert_eq!(queue.consecutive_wakes, 2);
    }
    #[test]
    fn paced_monitor_out_of_tokens_is_deferred_not_dropped() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line("one"), 0.0, &settings);
        assert!(queue.flush(0.0, &settings).is_some());
        let delay = queue.notify(line("two"), 10_000.0, &settings);
        assert!(delay > settings.coalesce_window_ms);
        assert!(queue.flush(10_000.0, &settings).is_none());
        let injection = queue.flush(3_700_000.0, &settings).unwrap();
        assert!(injection.content.contains("two"));
    }
    /// next_delay must agree with take_ready_wake: a summary skips both the rate-limit stamp and an
    /// exhausted pacer, so the loop is told to run at the 1 ms anti-spin floor, not in an hour.
    #[test]
    fn a_summary_on_an_exhausted_pacer_is_due_at_the_floor_while_a_line_waits() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line("one"), 0.0, &settings);
        assert!(
            queue.flush(0.0, &settings).is_some(),
            "spends the only token"
        );
        queue.notify(line("two"), 10.0, &settings);
        let line_delay = queue
            .next_delay(10.0, &settings)
            .expect("a line is pending");
        assert!(
            line_delay >= settings.rate_limit_ms,
            "a line on an exhausted pacer keeps waiting, got {line_delay}"
        );
        queue.notify(
            MonitorEvent::Summary {
                id: "b1".to_owned(),
                description: "watch".to_owned(),
                summary: "watcher completed".to_owned(),
            },
            10.0,
            &settings,
        );
        assert_eq!(queue.next_delay(10.0, &settings), Some(1.0));
        assert!(
            queue.flush(10.0, &settings).is_some(),
            "take_ready_wake agrees the summary is ready"
        );
    }
    #[test]
    fn retry_gate_still_defers_a_summary() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(
            MonitorEvent::Summary {
                id: "b1".to_owned(),
                description: "watch".to_owned(),
                summary: "watcher completed".to_owned(),
            },
            0.0,
            &settings,
        );
        queue.defer_retry(500.0);
        assert_eq!(queue.next_delay(10.0, &settings), Some(490.0));
    }
    /// Unchanged on purpose: a non-summary id with no rate-limit stamp (e.g. one fit_wake left out
    /// for character budget) still waits the full rate limit, not the 1 ms floor.
    #[test]
    fn an_unstamped_line_still_waits_the_rate_limit() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line("pending"), 0.0, &settings);
        assert!(!queue.last_injection.contains_key("b1"));
        assert_eq!(
            queue.next_delay(0.0, &settings),
            Some(settings.rate_limit_ms)
        );
    }
    #[test]
    fn details_carry_overflow_counts_and_human_input() {
        let mut book = OverflowBook::new(None);
        let event = line("x");
        book.record(&event, "x", true);
        let details = monitor_details(&[], &["b1".to_owned()], &book);
        assert_eq!(details[0]["eventCount"], 1);
        assert_eq!(details[0]["humanInput"], true);
        assert_eq!(details[0]["description"], "watch");
    }

    /// A send that admits every wake with a pending receipt; the tests settle the wake directly.
    fn pending_send() -> MonitorSend {
        std::sync::Arc::new(|injection: MonitorInjection| -> MonitorSendResult {
            Ok(MonitorAdmission {
                receipt: Some(maho_ext_api::MessageDeliveryReceipt::pending()),
                pause_ids: injection.pause_ids,
            })
        })
    }
    /// The weak settle sender `deliver` needs. These tests settle wakes directly through `settle`,
    /// so what a receipt waiter pushes into it is never read.
    fn settle_channel() -> (
        tokio::sync::mpsc::UnboundedSender<NotifierAction>,
        tokio::sync::mpsc::WeakUnboundedSender<NotifierAction>,
    ) {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let weak = sender.downgrade();
        (sender, weak)
    }
    fn counting_commit() -> (
        MonitorCommit,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let commit: MonitorCommit = std::sync::Arc::new(move |_: &[String]| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        (commit, calls)
    }

    /// REQUIRED interleaving regression (i): a line on a DIFFERENT monitor that arrives while wake
    /// A's receipt is outstanding survives A's `Failed` undo. A whole-snapshot rollback restores the
    /// pre-A queue and loses it.
    #[tokio::test(start_paused = true)]
    async fn a_failed_settle_keeps_lines_queued_on_another_monitor_meanwhile() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let (_open, settled) = settle_channel();
        let (commit, commits) = counting_commit();
        let report = |_: &str| {};
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line_on("a", "first"), 0.0, &settings);
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &pending_send(),
            Some(&commit),
            &settled,
            &report,
        )
        .expect("wake A is taken");
        queue.notify(line_on("b", "second"), 10.0, &settings);
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Failed("queue cleared".to_owned()),
            10.0,
            &settings,
            Some(&commit),
            &report,
        ));
        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 0);
        let retried = queue
            .flush(10.0 + settings.rate_limit_ms, &settings)
            .expect("the undone wake is retried");
        assert!(retried.content.contains("first"), "A's own line is back");
        assert!(
            retried.content.contains("second"),
            "B's line queued during A's receipt survives the undo"
        );
    }

    /// REQUIRED interleaving regression (ii): while wake A is outstanding the SAME paced monitor
    /// queues a line identical to one A took. A's `Failed` undo must keep both (a dedupe-by-content
    /// merge loses one, a whole-snapshot rollback loses the new one) and must give the token back
    /// arithmetically (a "restore if the bucket is unchanged" rule never fires, since every
    /// `ready_at` refill rewrites the bucket). At 1 wake/h the spend leaves 0 tokens, so `ready_at`
    /// can only report `now` if the token really came back.
    #[tokio::test(start_paused = true)]
    async fn a_failed_settle_keeps_an_identical_repeat_and_returns_the_token() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let (_open, settled) = settle_channel();
        let report = |_: &str| {};
        let mut queue = MonitorDeliveryQueue::with_pacing(Some(Box::new(|_| Some(1.0))), None);
        queue.notify(line_on("a", "tick"), 0.0, &settings);
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &pending_send(),
            None,
            &settled,
            &report,
        )
        .expect("wake A is taken");
        assert!(
            queue.ready_at("a", 10.0) > 10.0,
            "the wake spent the only token"
        );
        queue.notify(line_on("a", "tick"), 10.0, &settings);
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Failed("queue cleared".to_owned()),
            10.0,
            &settings,
            None,
            &report,
        ));
        assert_eq!(
            queue.ready_at("a", 10.0),
            10.0,
            "the spent token is returned"
        );
        let retried = queue
            .flush(10.0 + settings.rate_limit_ms, &settings)
            .expect("the undone wake is retried");
        assert_eq!(
            retried.content.matches("tick").count(),
            2,
            "both identical lines are delivered: {}",
            retried.content
        );
        assert_eq!(retried.details["monitors"][0]["eventCount"], 2);
    }

    /// PARTIAL-share regression: an overflow entry already fully spilled (`count == spilled > 0`)
    /// is retained by a prefix wake, so the undo must leave it exactly as it is. Adding the
    /// snapshot back would report the spilled lines twice in the notice.
    #[tokio::test(start_paused = true)]
    async fn a_failed_partial_share_leaves_its_retained_overflow_untouched() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        settings.max_lines_per_injection = 1.0;
        settings.max_chars_per_injection = 1024.0;
        // A path longer than the wake budget: the overflow notice cannot fit, so the monitor is
        // sent as a prefix of its lines (a partial share) and its overflow entry is retained.
        let path = format!("/{}", "p".repeat(1100));
        let spill_path = path.clone();
        let spill: MonitorSpill = Box::new(move |_, _| {
            Some(MonitorSpillResult {
                path: spill_path.clone(),
                rotated: true,
            })
        });
        let (_open, settled) = settle_channel();
        let report = |_: &str| {};
        let mut queue = MonitorDeliveryQueue::with_pacing(None, Some(spill));
        queue.notify(line_on("a", "inline"), 0.0, &settings);
        for index in 0..crate::monitor_overflow::SPILL_CHUNK_LINES {
            queue.notify(line_on("a", &format!("over {index}")), 0.0, &settings);
        }
        let before = queue.overflow.get("a").expect("overflow entry").clone();
        assert_eq!(before.count, crate::monitor_overflow::SPILL_CHUNK_LINES);
        assert_eq!(before.spilled, before.count, "fully spilled at take time");
        let wake = deliver(
            &mut queue,
            0.0,
            &settings,
            &pending_send(),
            None,
            &settled,
            &report,
        )
        .expect("wake A is taken");
        let PendingCommit { undo, .. } = queue
            .pending_commit
            .as_ref()
            .expect("wake A is outstanding");
        assert_eq!(undo.retained_overflow, ["a"], "the share was partial");
        assert!(undo.deleted_overflow.is_empty());
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Failed("queue cleared".to_owned()),
            10.0,
            &settings,
            None,
            &report,
        ));
        let after = queue.overflow.get("a").expect("overflow entry kept");
        assert!(after.spilled <= after.count);
        assert_eq!(after.count, before.count);
        assert_eq!(after.spilled, before.spilled, "spilled did not grow");
        assert_eq!(after.pending, before.pending);
        assert_eq!(after.path, before.path);
        assert_eq!(after.rotated, before.rotated);
        let retried = queue
            .flush(10.0 + settings.rate_limit_ms, &settings)
            .expect("the undone wake is retried");
        assert!(retried.content.contains("inline"), "the taken line is back");
    }

    /// BOUNDED regression: a host that refuses every wake is tried exactly
    /// [MONITOR_DELIVERY_ATTEMPTS] times, then the lines are dropped with one terminal diagnostic
    /// instead of being retried forever; a later committed wake resets the count.
    #[tokio::test(start_paused = true)]
    async fn an_always_refusing_host_costs_a_bounded_number_of_attempts() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let (_open, settled) = settle_channel();
        let diagnostics = std::cell::RefCell::new(Vec::<String>::new());
        let report = |line: &str| diagnostics.borrow_mut().push(line.to_owned());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let refusing: MonitorSend =
            std::sync::Arc::new(move |_: MonitorInjection| -> MonitorSendResult {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("host refused".to_owned())
            });
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("doomed"), 0.0, &settings);
        let mut now = 0.0;
        for _ in 0..10 {
            if deliver(
                &mut queue, now, &settings, &refusing, None, &settled, &report,
            )
            .is_none()
            {
                break;
            }
            now += settings.rate_limit_ms;
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            MONITOR_DELIVERY_ATTEMPTS as usize
        );
        let terminal = diagnostics
            .borrow()
            .iter()
            .filter(|line| line.contains("dropped after"))
            .count();
        assert_eq!(terminal, 1, "exactly one terminal diagnostic");
        assert!(queue.pending_ids().is_empty(), "the lines were dropped");
        assert_eq!(queue.retry_attempts, 0);
        // One more refusal counts again; a Delivered commit then resets the count.
        queue.notify(line("next"), now, &settings);
        deliver(
            &mut queue, now, &settings, &refusing, None, &settled, &report,
        )
        .expect("refused wake");
        assert_eq!(queue.retry_attempts, 1);
        now += settings.rate_limit_ms;
        let wake = deliver(
            &mut queue,
            now,
            &settings,
            &pending_send(),
            None,
            &settled,
            &report,
        )
        .expect("admitted wake");
        assert!(settle(
            &mut queue,
            wake,
            crate::notify::DeliveryOutcome::Delivered,
            now,
            &settings,
            None,
            &report,
        ));
        assert_eq!(queue.retry_attempts, 0, "a commit resets the count");
    }

    /// Settled failures count toward the same cap: the third `Failed` drops the wake once, and
    /// never invokes the commit hook.
    #[tokio::test(start_paused = true)]
    async fn settled_failures_count_toward_the_attempt_cap() {
        let settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor;
        let (_open, settled) = settle_channel();
        let (commit, commits) = counting_commit();
        let diagnostics = std::cell::RefCell::new(Vec::<String>::new());
        let report = |line: &str| diagnostics.borrow_mut().push(line.to_owned());
        let mut queue = MonitorDeliveryQueue::default();
        queue.notify(line("doomed"), 0.0, &settings);
        let mut now = 0.0;
        let mut attempts = 0;
        while let Some(wake) = deliver(
            &mut queue,
            now,
            &settings,
            &pending_send(),
            Some(&commit),
            &settled,
            &report,
        ) {
            attempts += 1;
            assert!(
                attempts <= MONITOR_DELIVERY_ATTEMPTS,
                "retried past the cap"
            );
            settle(
                &mut queue,
                wake,
                crate::notify::DeliveryOutcome::Failed("queue cleared".to_owned()),
                now,
                &settings,
                Some(&commit),
                &report,
            );
            now += settings.rate_limit_ms;
        }
        assert_eq!(attempts, MONITOR_DELIVERY_ATTEMPTS);
        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 0);
        let terminal = diagnostics
            .borrow()
            .iter()
            .filter(|line| line.contains("dropped after"))
            .count();
        assert_eq!(terminal, 1);
        assert!(queue.pending_ids().is_empty());
    }
}
