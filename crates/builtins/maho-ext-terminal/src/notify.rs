use crate::output_format::sanitize_terminal_output;
use crate::settings::NotifyMode;
use crate::tools::context::TerminalExitView;
use crate::tools::spawn::describe_exit;
use maho_ext_api::types::MessageDeliveryReceipt;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// What a delivery receipt says became of a message handed to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The receipt settled Ok: the message reached the session transcript.
    Delivered,
    /// The receipt settled Err: by contract the message will not be delivered, so a retry is safe.
    Failed(String),
    /// The receipt did not settle in time (or was dropped unsettled). Abandoning the waiter does not
    /// withdraw the queued message, so it may still land: callers must commit and diagnose, and
    /// must NEVER retry on this outcome, or one notice can be delivered twice.
    Indeterminate,
}

/// How long a notice may sit unconfirmed before its waiter reports [DeliveryOutcome::Indeterminate].
///
/// This caps how long a notice waits for confirmation; it does not claim a turn settles inside it.
/// A Steer delivery can stay pending for a whole long turn (`defer_trigger_turn`), so the timeout
/// is expected to fire during long turns. That is safe because `Indeterminate` commits and is
/// diagnosed, never retried: a notice that lands late is still delivered exactly once.
///
/// ACCEPTED STALL: the monitor notifier holds at most one wake outstanding until its receipt
/// settles, so a long turn can stall monitor wakes for up to this long. That is a deliberate,
/// documented consequence of exactly-once delivery, not an accident.
pub const RECEIPT_ABANDON_AFTER: Duration = Duration::from_secs(30);

fn settled_outcome(outcome: Result<(), String>) -> DeliveryOutcome {
    match outcome {
        Ok(()) => DeliveryOutcome::Delivered,
        Err(reason) => DeliveryOutcome::Failed(reason),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where a delivery diagnostic goes: `maho_ext_api::diagnostic` in production, a capture in tests.
type Report = Arc<dyn Fn(&str) + Send + Sync>;

fn diagnostic_report() -> Report {
    Arc::new(maho_ext_api::diagnostic)
}

/// Calls `on_settle` exactly once with what `receipt` says about delivery: `Delivered` or
/// `Failed` when it settles, `Indeterminate` when it has not settled after `abandon_after`.
///
/// `on_settle` may run on whichever thread settles the receipt, so it must be quick and must not
/// block. A receipt that settles Err after the waiter gave up is still reported as a diagnostic
/// (never as a second `on_settle`). Without a Tokio runtime nothing is spawned: an already-settled
/// receipt is reported synchronously and an unsettled one is reported `Indeterminate` at once.
pub fn spawn_receipt_wait(
    receipt: MessageDeliveryReceipt,
    on_settle: impl FnOnce(DeliveryOutcome) + Send + 'static,
    abandon_after: Duration,
) {
    wait_for_receipt(receipt, on_settle, abandon_after, diagnostic_report());
}

fn wait_for_receipt<F>(
    receipt: MessageDeliveryReceipt,
    on_settle: F,
    abandon_after: Duration,
    report: Report,
) where
    F: FnOnce(DeliveryOutcome) + Send + 'static,
{
    // Both the settle path and the give-up path take() the callback; whichever is first runs it.
    let slot = Arc::new(Mutex::new(Some(on_settle)));
    let (settled_tx, settled_rx) = tokio::sync::oneshot::channel::<()>();
    let settle_slot = slot.clone();
    receipt.on_settled(move |outcome| {
        let callback = lock(&settle_slot).take();
        match callback {
            Some(callback) => {
                // Only wakes the waiter early; a waiter that is already gone needs no wake.
                let _ = settled_tx.send(());
                callback(settled_outcome(outcome));
            }
            None => {
                if let Err(reason) = outcome {
                    report(&format!(
                        "message delivery failed after it was reported unconfirmed: {reason}"
                    ));
                }
            }
        }
    });
    if lock(&slot).is_none() {
        // Already settled: on_settled ran the callback synchronously, there is nothing to await.
        return;
    }
    let give_up = move || {
        let callback = lock(&slot).take();
        if let Some(callback) = callback {
            callback(DeliveryOutcome::Indeterminate);
        }
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        // An already-settled receipt ran the callback inside on_settled above; an unsettled one
        // cannot be awaited without a runtime and is unconfirmed now.
        give_up();
        return;
    };
    handle.spawn(async move {
        // Err(elapsed) is the timeout; Ok(Err(_)) is a receipt dropped without ever settling.
        if !matches!(
            tokio::time::timeout(abandon_after, settled_rx).await,
            Ok(Ok(()))
        ) {
            give_up();
        }
    });
}

pub const NON_INTERACTIVE_MODES: [&str; 2] = ["print", "json"];
pub const TERMINAL_NOTIFICATION_CUSTOM_TYPE: &str = "senpi-terminal:notification";
pub const NOTICE_TAIL_MAX_CHARS: usize = 2000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationDelivery {
    Steer,
    FollowUp,
}
pub fn get_terminal_notification_delivery(
    notify_mode: NotifyMode,
    context_mode: Option<&str>,
    has_model: bool,
    force_wake: bool,
) -> Option<NotificationDelivery> {
    let mode = context_mode?;
    if notify_mode == NotifyMode::Off || NON_INTERACTIVE_MODES.contains(&mode) || !has_model {
        return None;
    }
    Some(if notify_mode == NotifyMode::Wake || force_wake {
        NotificationDelivery::Steer
    } else {
        NotificationDelivery::FollowUp
    })
}
pub fn build_notice(id: &str, view: &TerminalExitView) -> String {
    let exit = view.exit_result.as_ref();
    let output = view.full_output();
    let status = describe_exit(exit).unwrap_or_else(|| "exited".to_owned());
    let code_text = exit
        .and_then(|exit| exit.exit_code)
        .map(|code| format!(" (exit code {code})"))
        .unwrap_or_default();
    let sanitized = sanitize_terminal_output(output);
    let tail = sanitized.trim_end();
    let units = tail.encode_utf16().collect::<Vec<_>>();
    let tail_section = if tail.is_empty() {
        String::new()
    } else {
        let truncated = units.len() > NOTICE_TAIL_MAX_CHARS;
        let shown = if truncated {
            String::from_utf16_lossy(&units[units.len() - NOTICE_TAIL_MAX_CHARS..])
        } else {
            tail.to_owned()
        };
        let note = if truncated {
            format!(
                "\n[Final output truncated to the last {NOTICE_TAIL_MAX_CHARS} chars; the full history is still peekable.]"
            )
        } else {
            String::new()
        };
        format!("\nFinal output:\n{shown}{note}")
    };
    format!(
        "<system-reminder>Background terminal session {id} finished: {status}{code_text}.{tail_section}</system-reminder>"
    )
}
/// Once-only completion notices. An id is `pending` from the send until its receipt settles, then
/// moves to `notified` (delivered or unconfirmed) or is released (failed, so the next report of the
/// same exit offers it again). Lock order is always `pending` then `notified`.
pub struct TerminalNotifier {
    notified: Arc<Mutex<HashSet<String>>>,
    pending: Arc<Mutex<HashSet<String>>>,
    report: Report,
}
impl Default for TerminalNotifier {
    fn default() -> Self {
        Self {
            notified: Default::default(),
            pending: Default::default(),
            report: diagnostic_report(),
        }
    }
}
/// Settles one in-flight notice: `notified` gains the id before `pending` loses it, both under the
/// `pending` lock, so an entry check never sees the id in neither set while it is being moved.
fn settle_notice(
    pending: &Mutex<HashSet<String>>,
    notified: &Mutex<HashSet<String>>,
    id: &str,
    outcome: DeliveryOutcome,
    report: &Report,
) {
    let note = {
        let mut pending = lock(pending);
        let note = match outcome {
            DeliveryOutcome::Delivered => {
                lock(notified).insert(id.to_owned());
                None
            }
            // The message may still land: re-sending would announce one exit twice.
            DeliveryOutcome::Indeterminate => {
                lock(notified).insert(id.to_owned());
                Some(format!(
                    "terminal notification for {id} unconfirmed after {}s: kept as delivered and not resent, since it may still land",
                    RECEIPT_ABANDON_AFTER.as_secs()
                ))
            }
            DeliveryOutcome::Failed(reason) => Some(format!(
                "terminal notification for {id} failed after admission: {reason}; the exit stays eligible for its next report"
            )),
        };
        pending.remove(id);
        note
    };
    if let Some(note) = note {
        report(&note);
    }
}
impl TerminalNotifier {
    pub fn notify_completion(
        &mut self,
        id: &str,
        delivery: Option<NotificationDelivery>,
        view: &TerminalExitView,
        mut send: impl FnMut(String, NotificationDelivery) -> Result<MessageDeliveryReceipt, String>,
    ) {
        let Some(delivery) = delivery else {
            return;
        };
        {
            let mut pending = lock(&self.pending);
            if pending.contains(id) || lock(&self.notified).contains(id) {
                return;
            }
            pending.insert(id.to_owned());
        }
        // Once-only means once delivered: a refused notice stays eligible, or the exit is announced
        // nowhere and nothing replays it.
        let receipt = match send(build_notice(id, view), delivery) {
            Ok(receipt) => receipt,
            Err(_) => {
                lock(&self.pending).remove(id);
                return;
            }
        };
        let report = self.report.clone();
        match receipt.outcome() {
            Some(outcome) => settle_notice(
                &self.pending,
                &self.notified,
                id,
                settled_outcome(outcome),
                &report,
            ),
            None => {
                let (pending, notified, id) =
                    (self.pending.clone(), self.notified.clone(), id.to_owned());
                wait_for_receipt(
                    receipt,
                    move |outcome| settle_notice(&pending, &notified, &id, outcome, &report),
                    RECEIPT_ABANDON_AFTER,
                    self.report.clone(),
                );
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> TerminalExitView {
        TerminalExitView::new(None, String::new())
    }
    #[test]
    fn guards_and_modes() {
        for mode in NON_INTERACTIVE_MODES {
            assert_eq!(
                get_terminal_notification_delivery(NotifyMode::Wake, Some(mode), true, false),
                None
            );
        }
        assert_eq!(
            get_terminal_notification_delivery(NotifyMode::Off, Some("interactive"), true, true),
            None
        );
        assert_eq!(
            get_terminal_notification_delivery(NotifyMode::Wake, Some("interactive"), false, false),
            None
        );
        assert_eq!(
            get_terminal_notification_delivery(
                NotifyMode::NextTurn,
                Some("interactive"),
                true,
                false
            ),
            Some(NotificationDelivery::FollowUp)
        );
        assert_eq!(
            get_terminal_notification_delivery(
                NotifyMode::NextTurn,
                Some("interactive"),
                true,
                true
            ),
            Some(NotificationDelivery::Steer)
        );
    }
    #[test]
    fn once_only_after_deliverable() {
        let mut notifier = TerminalNotifier::default();
        let mut sent = Vec::new();
        notifier.notify_completion("bash_1", None, &view(), |text, _| {
            sent.push(text);
            Ok(MessageDeliveryReceipt::delivered())
        });
        notifier.notify_completion(
            "bash_1",
            Some(NotificationDelivery::Steer),
            &view(),
            |text, _| {
                sent.push(text);
                Ok(MessageDeliveryReceipt::delivered())
            },
        );
        notifier.notify_completion(
            "bash_1",
            Some(NotificationDelivery::Steer),
            &view(),
            |text, _| {
                sent.push(text);
                Ok(MessageDeliveryReceipt::delivered())
            },
        );
        assert_eq!(sent.len(), 1);
    }

    /// The notice is burned by a *delivered* message only: a host that refuses it must leave the
    /// exit announced nowhere (no replay path exists) unless the id stays eligible.
    #[test]
    fn a_refused_notice_stays_undelivered() {
        let mut notifier = TerminalNotifier::default();
        notifier.notify_completion(
            "bash_2",
            Some(NotificationDelivery::Steer),
            &view(),
            |_, _| Err("session is shutting down".to_owned()),
        );
        let mut sent = Vec::new();
        notifier.notify_completion(
            "bash_2",
            Some(NotificationDelivery::Steer),
            &view(),
            |text, _| {
                sent.push(text);
                Ok(MessageDeliveryReceipt::delivered())
            },
        );
        assert_eq!(
            sent.len(),
            1,
            "the retry delivers the notice it could not send"
        );
        let mut again = Vec::new();
        notifier.notify_completion(
            "bash_2",
            Some(NotificationDelivery::Steer),
            &view(),
            |text, _| {
                again.push(text);
                Ok(MessageDeliveryReceipt::delivered())
            },
        );
        assert!(again.is_empty(), "a delivered notice is once-only");
    }

    type Outcomes = tokio::sync::mpsc::UnboundedReceiver<DeliveryOutcome>;

    /// An `on_settle` that forwards every call, so a test counts them and awaits the exact one.
    fn outcome_channel() -> (impl FnOnce(DeliveryOutcome) + Send + 'static, Outcomes) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            move |outcome| {
                sender.send(outcome).expect("outcome receiver");
            },
            receiver,
        )
    }

    /// A diagnostic sink that records each line and signals it.
    fn capture_report() -> (
        Report,
        Arc<Mutex<Vec<String>>>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let (signal, signals) = tokio::sync::mpsc::unbounded_channel();
        let recorded = lines.clone();
        let report: Report = Arc::new(move |line: &str| {
            lock(&recorded).push(line.to_owned());
            // The test may have stopped listening; the recorded line is what it asserts on.
            let _ = signal.send(());
        });
        (report, lines, signals)
    }

    #[tokio::test(start_paused = true)]
    async fn a_delivered_receipt_reports_exactly_one_delivered() {
        let receipt = MessageDeliveryReceipt::pending();
        let (on_settle, mut outcomes) = outcome_channel();
        spawn_receipt_wait(receipt.clone(), on_settle, RECEIPT_ABANDON_AFTER);
        receipt.resolve_delivered();
        assert_eq!(outcomes.recv().await, Some(DeliveryOutcome::Delivered));
        // Past the deadline the waiter has finished and dropped nothing further into the channel.
        tokio::time::advance(RECEIPT_ABANDON_AFTER * 2).await;
        tokio::task::yield_now().await;
        assert!(outcomes.try_recv().is_err(), "no second callback");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unsettled_receipt_reports_indeterminate_once_at_the_deadline() {
        let receipt = MessageDeliveryReceipt::pending();
        let (on_settle, mut outcomes) = outcome_channel();
        let started = tokio::time::Instant::now();
        spawn_receipt_wait(receipt.clone(), on_settle, RECEIPT_ABANDON_AFTER);
        let outcome = tokio::time::timeout(RECEIPT_ABANDON_AFTER * 2, outcomes.recv())
            .await
            .expect("the waiter gives up inside the window");
        assert_eq!(outcome, Some(DeliveryOutcome::Indeterminate));
        assert!(started.elapsed() >= RECEIPT_ABANDON_AFTER);
        assert!(started.elapsed() < RECEIPT_ABANDON_AFTER * 2);
        assert!(outcomes.try_recv().is_err(), "exactly one callback");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_after_the_deadline_is_diagnosed_not_reported_twice() {
        let receipt = MessageDeliveryReceipt::pending();
        let (on_settle, mut outcomes) = outcome_channel();
        let (report, lines, _signals) = capture_report();
        wait_for_receipt(receipt.clone(), on_settle, RECEIPT_ABANDON_AFTER, report);
        assert_eq!(outcomes.recv().await, Some(DeliveryOutcome::Indeterminate));
        receipt.resolve_failed("dropped on reload");
        assert!(
            outcomes.try_recv().is_err(),
            "the late settle must not call on_settle again"
        );
        let lines = lock(&lines).clone();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("dropped on reload"), "{lines:?}");
    }

    /// A caller that retries only on `Failed` must not resend an unconfirmed notice, and the same
    /// receipt landing late still leaves exactly one recorded delivery.
    #[tokio::test(start_paused = true)]
    async fn an_indeterminate_notice_is_not_resent_and_a_late_delivery_records_once() {
        let (report, lines, mut signals) = capture_report();
        let mut notifier = TerminalNotifier {
            report,
            ..TerminalNotifier::default()
        };
        let mut receipts = Vec::new();
        let offer = |notifier: &mut TerminalNotifier,
                     receipts: &mut Vec<MessageDeliveryReceipt>| {
            notifier.notify_completion(
                "bash_3",
                Some(NotificationDelivery::Steer),
                &view(),
                |_, _| {
                    let receipt = MessageDeliveryReceipt::pending();
                    receipts.push(receipt.clone());
                    Ok(receipt)
                },
            );
        };
        for _ in 0..3 {
            offer(&mut notifier, &mut receipts);
        }
        assert_eq!(receipts.len(), 1, "in-flight notice is not resent");
        tokio::time::timeout(RECEIPT_ABANDON_AFTER * 2, signals.recv())
            .await
            .expect("the unconfirmed notice is diagnosed")
            .expect("report sender");
        assert!(lock(&notifier.notified).contains("bash_3"));
        assert!(lock(&notifier.pending).is_empty());
        for _ in 0..3 {
            offer(&mut notifier, &mut receipts);
        }
        assert_eq!(receipts.len(), 1, "an unconfirmed notice is never retried");
        receipts[0].resolve_delivered();
        assert_eq!(lock(&notifier.notified).len(), 1);
        offer(&mut notifier, &mut receipts);
        assert_eq!(receipts.len(), 1);
        let lines = lock(&lines).clone();
        assert_eq!(lines.len(), 1, "{lines:?}");
    }

    #[test]
    fn without_a_runtime_the_waiter_reports_without_panicking() {
        let seen = Arc::new(Mutex::new(Vec::<DeliveryOutcome>::new()));
        let record = |seen: &Arc<Mutex<Vec<DeliveryOutcome>>>| {
            let seen = seen.clone();
            move |outcome: DeliveryOutcome| lock(&seen).push(outcome)
        };
        spawn_receipt_wait(
            MessageDeliveryReceipt::pending(),
            record(&seen),
            RECEIPT_ABANDON_AFTER,
        );
        assert_eq!(*lock(&seen), [DeliveryOutcome::Indeterminate]);
        spawn_receipt_wait(
            MessageDeliveryReceipt::delivered(),
            record(&seen),
            RECEIPT_ABANDON_AFTER,
        );
        assert_eq!(
            *lock(&seen),
            [DeliveryOutcome::Indeterminate, DeliveryOutcome::Delivered]
        );
    }

    #[tokio::test]
    async fn a_notice_whose_receipt_fails_stays_eligible() {
        let mut notifier = TerminalNotifier::default();
        let mut receipts = Vec::new();
        let offer = |notifier: &mut TerminalNotifier,
                     receipts: &mut Vec<MessageDeliveryReceipt>| {
            notifier.notify_completion(
                "bash_4",
                Some(NotificationDelivery::FollowUp),
                &view(),
                |_, _| {
                    let receipt = MessageDeliveryReceipt::pending();
                    receipts.push(receipt.clone());
                    Ok(receipt)
                },
            );
        };
        offer(&mut notifier, &mut receipts);
        receipts[0].resolve_failed("queue cleared");
        assert!(lock(&notifier.pending).is_empty());
        assert!(lock(&notifier.notified).is_empty());
        offer(&mut notifier, &mut receipts);
        assert_eq!(receipts.len(), 2, "a failed notice is offered again");
        let mut settled_failed = Vec::new();
        notifier.notify_completion(
            "bash_5",
            Some(NotificationDelivery::Steer),
            &view(),
            |text, _| {
                settled_failed.push(text);
                Ok(MessageDeliveryReceipt::failed("refused at admission"))
            },
        );
        notifier.notify_completion(
            "bash_5",
            Some(NotificationDelivery::Steer),
            &view(),
            |text, _| {
                settled_failed.push(text);
                Ok(MessageDeliveryReceipt::failed("refused at admission"))
            },
        );
        assert_eq!(
            settled_failed.len(),
            2,
            "an already-failed receipt leaves it eligible"
        );
    }

    #[tokio::test]
    async fn a_delivered_receipt_sends_once_across_three_calls() {
        let mut notifier = TerminalNotifier::default();
        let mut receipts = Vec::new();
        for round in 0..3 {
            notifier.notify_completion(
                "bash_6",
                Some(NotificationDelivery::Steer),
                &view(),
                |_, _| {
                    let receipt = MessageDeliveryReceipt::pending();
                    receipts.push(receipt.clone());
                    Ok(receipt)
                },
            );
            if round == 0 {
                receipts[0].resolve_delivered();
            }
        }
        assert_eq!(receipts.len(), 1);
        assert!(lock(&notifier.notified).contains("bash_6"));
    }

    /// Three exit reports racing one unsettled receipt send once: the pending set, not the
    /// delivered set, is what blocks the duplicates.
    #[tokio::test]
    async fn three_rapid_calls_with_an_unsettled_receipt_send_once() {
        let mut notifier = TerminalNotifier::default();
        let mut sent = 0;
        let receipt = MessageDeliveryReceipt::pending();
        for _ in 0..3 {
            notifier.notify_completion(
                "bash_7",
                Some(NotificationDelivery::Steer),
                &view(),
                |_, _| {
                    sent += 1;
                    Ok(receipt.clone())
                },
            );
        }
        assert_eq!(sent, 1);
        assert!(lock(&notifier.pending).contains("bash_7"));
        assert!(lock(&notifier.notified).is_empty());
    }
}
