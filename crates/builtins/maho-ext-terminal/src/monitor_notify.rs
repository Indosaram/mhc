use crate::monitor_notify_format::{
    WAKE_BUDGET_PAUSE_NOTICE, build_wake_message, event_body, rendered_length,
};
pub use crate::monitor_overflow::MonitorSpillResult;
use crate::monitor_overflow::{EventKind, MonitorSpill, OverflowBook};
use crate::monitor_registry::MonitorEvent;
use crate::monitor_wake_fit::fit_wake;
use crate::monitor_wake_pacing::WakePacer;
use std::collections::BTreeSet;
pub const MONITOR_NOTIFICATION_CUSTOM_TYPE: &str = "senpi-monitor:notification";
const QUEUE_OVERHEAD_CHARS: usize = 512;
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
}
impl MonitorNotifier {
    /// `send` returns what the host said about the injection; a refusal is rolled back into the
    /// queue instead of being reported on stderr and forgotten.
    pub fn new(
        settings: crate::settings::MonitorDeliverySettings,
        send: impl Fn(MonitorInjection) -> MonitorSendResult + Send + 'static,
    ) -> Self {
        Self::with_delivery_guard(settings, || true, send)
    }
    pub fn with_delivery_guard(
        settings: crate::settings::MonitorDeliverySettings,
        available: impl Fn() -> bool + Send + 'static,
        send: impl Fn(MonitorInjection) -> MonitorSendResult + Send + 'static,
    ) -> Self {
        Self::with_pacing(settings, available, None, None, send)
    }
    /// `wakes_per_hour` and `spill_overflow` are upstream `MonitorNotifierDeps.wakesPerHour` / `spillOverflow`.
    pub fn with_pacing(
        settings: crate::settings::MonitorDeliverySettings,
        available: impl Fn() -> bool + Send + 'static,
        wakes_per_hour: Option<WakesPerHour>,
        spill_overflow: Option<MonitorSpill>,
        send: impl Fn(MonitorInjection) -> MonitorSendResult + Send + 'static,
    ) -> Self {
        let settings = resolve_delivery_settings(&settings);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
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
                        deliver(&mut queue,now,&settings,&send);
                        deadline=queue.next_delay(now,&settings).map(after);
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
                        let due = after(delay);
                        deadline =
                            Some(deadline.map_or(due, |old: tokio::time::Instant| old.min(due)));
                    }
                    Some(NotifierAction::Activity) => queue.note_activity(),
                    Some(NotifierAction::Resume(ids)) => queue.resume(&ids),
                    None => return,
                }
            }
        });
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
fn deliver(
    queue: &mut MonitorDeliveryQueue,
    now: f64,
    settings: &crate::settings::MonitorDeliverySettings,
    send: &impl Fn(MonitorInjection) -> MonitorSendResult,
) {
    let Some((injection, checkpoint)) = queue.flush_with_checkpoint(now, settings) else {
        return;
    };
    let pause_ids = injection.pause_ids.clone();
    match send(injection) {
        Ok(()) => {}
        Err(error) => {
            queue.rollback(checkpoint);
            queue.defer_retry(now + settings.rate_limit_ms);
            maho_ext_api::diagnostic(&format!(
                "monitor wake not delivered, requeued for the next pass: {error}"
            ));
            if !pause_ids.is_empty() {
                maho_ext_api::diagnostic(&format!(
                    "monitor wake budget pause not applied to {}: the wake was not delivered",
                    pause_ids.join(", ")
                ));
            }
        }
    }
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
/// What the host reports for one wake it was handed.
pub type MonitorSendResult = Result<(), String>;
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
        }
    }
    pub fn dispose(&mut self) {
        self.events.clear();
        self.overflow.clear();
        self.last_batch.clear();
        self.pacer.clear();
        self.retry_not_before = None;
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
        let earliest = ids
            .iter()
            .map(|id| {
                let ready = self.ready_at(id, now);
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
    pub fn flush(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<MonitorInjection> {
        self.take_wake(now, settings)
    }

    /// Takes the next wake out of the queue together with the state needed to put it back, so a
    /// refused delivery can be undone with [Self::rollback] instead of dropping the lines.
    pub fn flush_with_checkpoint(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<(MonitorInjection, MonitorCheckpoint)> {
        let checkpoint = self.checkpoint();
        let injection = self.take_wake(now, settings)?;
        Some((injection, checkpoint))
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
    ) -> Option<MonitorInjection> {
        if self.retry_not_before.is_some_and(|at| now < at) {
            return None;
        }
        // The gate is spent the moment it elapses, even with nothing left to send: keeping it would
        // make next_delay report a delay forever and spin the notifier on an empty queue.
        self.retry_not_before = None;
        self.take_ready_wake(now, settings)
    }

    fn take_ready_wake(
        &mut self,
        now: f64,
        settings: &crate::settings::MonitorDeliverySettings,
    ) -> Option<MonitorInjection> {
        let pending = self.pending_ids();
        if pending.is_empty() {
            return None;
        }
        let summary_ids = self
            .events
            .iter()
            .filter(|event| identity(event).2 == "summary")
            .map(|event| identity(event).0.to_owned())
            .collect::<BTreeSet<_>>();
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
            }
            self.last_injection.insert(id.clone(), now);
            // A monitor that sent only a prefix keeps its remaining lines and overflow for the next wake.
            if share.whole {
                self.overflow.delete(id);
            }
            match if share.whole {
                fingerprints.remove(id)
            } else {
                None
            } {
                Some(batch) => {
                    self.last_batch.insert(id.clone(), batch);
                }
                None => {
                    self.last_batch.remove(id);
                }
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
        Some(MonitorInjection {
            content,
            details,
            pause_ids: if reaches_budget { injected } else { vec![] },
        })
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
            move |injection| sender.send(injection).map_err(|error| error.to_string()),
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
        let notifier = MonitorNotifier::new(settings, move |injection| {
            sender.send(injection).map_err(|error| error.to_string())
        });
        notifier.notify_event(line("ready")).unwrap();
        let injection = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(injection.content.contains("Monitor event(watch): ready"));
        assert_eq!(injection.details["monitors"][0]["eventCount"], 1);
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
        let (injection, checkpoint) = queue
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
        let (_, checkpoint) = queue
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
        let (_, checkpoint) = queue
            .flush_with_checkpoint(0.0, &settings)
            .expect("wake is ready");
        queue.rollback(checkpoint);
        assert_eq!(queue.consecutive_wakes, 0);
        let retried = queue
            .flush(0.0, &settings)
            .expect("the rolled-back wake is delivered again");
        assert_eq!(retried.pause_ids, ["b1"]);
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
}
