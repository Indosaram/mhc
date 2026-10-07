use super::*;
use memory_core::identity::layout::build_identity_paths;
use memory_core::recall::RecallLedger;
use std::sync::Mutex as StdMutex;

fn context(root: &std::path::Path) -> MemoryIdentityContext {
    MemoryIdentityContext::new("agent".into(), build_identity_paths(root, "agent"), crate::binding::MemorySessionBinding { identity: "agent".into(), repo_path_hash: "hash".into(), bound_at: 0.0 })
}

fn nudge(path: &str, hint: &str) -> RecallNudge { RecallNudge { path: path.into(), hint: hint.into() } }

struct RealPending(memory_core::recall::PendingNudges);
impl KibitzerPendingPort for RealPending {
    fn write(&self, session_id: &str, nudges: &[RecallNudge]) -> std::io::Result<()> { self.0.write(session_id, nudges) }
    fn delete(&self, session_id: &str) { self.0.delete(session_id); }
}

#[derive(Default)]
struct Recorder {
    enqueued: StdMutex<Vec<String>>,
    removed: StdMutex<Vec<String>>,
    sent: StdMutex<Vec<String>>,
    entries: StdMutex<Vec<serde_json::Value>>,
    warns: StdMutex<Vec<String>>,
    reenter: StdMutex<Option<Arc<KibitzerDelivery>>>,
    /// The fixture root, set by `delivery()`. Carried here so the re-entrant coordinators rewrite
    /// pending under the SAME tempdir the test owns - never a real ledger on the host.
    root: StdMutex<Option<std::path::PathBuf>>,
}

/// A coordinator whose `enqueue` re-enters `mark_delivered` (proves no deadlock: callbacks run
/// outside every delivery lock). The fixture root travels WITH the coordinator, so the re-entrant
/// `mark_delivered` rewrites pending under the SAME tempdir the test owns - never a real ledger.
struct ReentrantCoordinator { recorder: Arc<Recorder>, root: std::path::PathBuf }
impl KibitzerIdleCoordinator for ReentrantCoordinator {
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool {
        self.recorder.enqueued.lock().unwrap().push(entry.key.clone());
        if let Some(delivery) = self.recorder.reenter.lock().unwrap().clone() {
            let context = context(&self.root);
            // Re-entrant flush of a DIFFERENT path; must not deadlock on any delivery lock.
            delivery.mark_delivered("s1", &context, &["other.md".to_string()], KibitzerDeliveryVia::Wake);
        }
        true
    }
    fn remove(&self, key: &str) { self.recorder.removed.lock().unwrap().push(key.to_string()); }
}

fn delivery(root: &std::path::Path, coordinator: Option<Arc<dyn KibitzerIdleCoordinator>>, recorder: Arc<Recorder>) -> Arc<KibitzerDelivery> {
    let ledger_dir = root.join("ledger");
    let pending_dir = root.join("pending");
    let sent = Arc::clone(&recorder);
    let entries = Arc::clone(&recorder);
    let warns = Arc::clone(&recorder);
    let delivery = KibitzerDelivery::new(KibitzerDeliveryOptions {
        ledger_for: Arc::new(move |_| RecallLedger::new(ledger_dir.clone())),
        pending_for: Arc::new(move |_| Arc::new(RealPending(memory_core::recall::PendingNudges::new(pending_dir.clone()))) as Arc<dyn KibitzerPendingPort>),
        coordinator,
        send_message: Arc::new(move |message| { sent.sent.lock().unwrap().push(message.content); Ok(()) }),
        append_entry: Arc::new(move |_kind, data| { entries.entries.lock().unwrap().push(data); }),
        warn: Arc::new(move |message| { warns.warns.lock().unwrap().push(message.to_string()); }),
    });
    *recorder.reenter.lock().unwrap() = Some(Arc::clone(&delivery));
    *recorder.root.lock().unwrap() = Some(root.to_path_buf());
    delivery
}

fn pending_paths(root: &std::path::Path) -> std::collections::BTreeSet<String> {
    memory_core::recall::PendingNudges::new(root.join("pending")).take("s1").into_iter().map(|nudge| nudge.path).collect()
}

#[test]
fn given_two_accepts_before_a_drain_when_pending_is_read_then_it_holds_the_merged_map() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let delivery = delivery(root.path(), None, Arc::new(Recorder::default()));
    delivery.accept("s1", &context, &[nudge("a.md", "first")]);
    delivery.accept("s1", &context, &[nudge("b.md", "second")]);
    assert_eq!(pending_paths(root.path()), std::collections::BTreeSet::from(["a.md".to_string(), "b.md".to_string()]));
}

#[test]
fn given_a_reentrant_coordinator_when_accepting_then_it_does_not_deadlock() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), Some(Arc::new(ReentrantCoordinator { recorder: Arc::clone(&recorder), root: root.path().to_path_buf() })), Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    assert!(recorder.enqueued.lock().unwrap().contains(&"kibitzer:a.md".to_string()));
}

/// A coordinator whose `remove` re-enters `accept` for one extra path. `drain_for_prompt` runs the
/// `remove` callbacks OUTSIDE the state lock, so this lands exactly in the window where an
/// unconditional `sessions.remove` would drop the reentrant nudges.
struct RemoveReentrantCoordinator { recorder: Arc<Recorder>, extra: String }
impl KibitzerIdleCoordinator for RemoveReentrantCoordinator {
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool { self.recorder.enqueued.lock().unwrap().push(entry.key.clone()); true }
    fn remove(&self, key: &str) {
        self.recorder.removed.lock().unwrap().push(key.to_string());
        if let Some(delivery) = self.recorder.reenter.lock().unwrap().clone() {
            let root = self.recorder.root.lock().unwrap().clone().expect("fixture root");
            let context = context(&root);
            delivery.accept("s1", &context, &[nudge(&self.extra, "reentrant")]);
        }
    }
}

/// Deterministic interleaving via the coordinator callback (no sleeps, no threads): the drain has
/// already cleared `a.md` and released the state lock when the reentrant accept of `b.md` lands.
#[test]
fn given_a_reentrant_accept_during_drain_when_it_finishes_then_the_new_accept_is_not_lost() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let coordinator = Arc::new(RemoveReentrantCoordinator { recorder: Arc::clone(&recorder), extra: "b.md".to_string() });
    let delivery = delivery(root.path(), Some(coordinator), Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "first")]);
    let drained: Vec<String> = delivery.drain_for_prompt("s1").into_iter().map(|nudge| nudge.path).collect();
    assert_eq!(drained, vec!["a.md".to_string()]);
    assert_eq!(pending_paths(root.path()), std::collections::BTreeSet::from(["b.md".to_string()]));
}

/// Deterministic interleaving via the `send_message` callback: the first steer has already claimed
/// `steering` under the lock and is mid-dispatch when a second steer enters. Without the atomic
/// claim the second one re-dispatches the same set and the message is sent twice.
#[test]
fn given_two_steers_overlapping_when_the_first_is_mid_dispatch_then_only_one_message_is_sent() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let sent: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let reenter: Arc<StdMutex<Option<Arc<KibitzerDelivery>>>> = Arc::new(StdMutex::new(None));
    let sent_cb = Arc::clone(&sent);
    let reenter_cb = Arc::clone(&reenter);
    let context_cb = context.clone();
    let ledger_dir = root.path().join("ledger");
    let pending_dir = root.path().join("pending");
    let delivery = KibitzerDelivery::new(KibitzerDeliveryOptions {
        ledger_for: Arc::new(move |_| RecallLedger::new(ledger_dir.clone())),
        pending_for: Arc::new(move |_| Arc::new(RealPending(memory_core::recall::PendingNudges::new(pending_dir.clone()))) as Arc<dyn KibitzerPendingPort>),
        coordinator: None,
        send_message: Arc::new(move |message| {
            sent_cb.lock().unwrap().push(message.content);
            if let Some(delivery) = reenter_cb.lock().unwrap().clone() {
                delivery.steer("s1", &context_cb);
            }
            Ok(())
        }),
        append_entry: Arc::new(|_kind, _data| {}),
        warn: Arc::new(|_| {}),
    });
    *reenter.lock().unwrap() = Some(Arc::clone(&delivery));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    delivery.steer("s1", &context);
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[test]
fn given_a_running_session_with_a_tool_in_flight_when_accepting_then_it_steers_at_once() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), None, Arc::clone(&recorder));
    delivery.mark_running("s1");
    delivery.mark_tool_started("s1", "call-1");
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    assert_eq!(recorder.sent.lock().unwrap().len(), 1);
}

#[test]
fn given_an_idle_session_when_accepting_then_it_does_not_steer() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), None, Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    assert!(recorder.sent.lock().unwrap().is_empty());
}

#[test]
fn given_the_gate_open_when_on_tool_result_runs_then_it_steers() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), None, Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    delivery.on_tool_result("s1", &context, &KibitzerToolResultGate { has_pending_messages: false, is_idle: false });
    assert_eq!(recorder.sent.lock().unwrap().len(), 1);
}

#[test]
fn given_pending_messages_or_idle_when_on_tool_result_runs_then_it_stays_held() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), None, Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    delivery.on_tool_result("s1", &context, &KibitzerToolResultGate { has_pending_messages: true, is_idle: false });
    delivery.on_tool_result("s1", &context, &KibitzerToolResultGate { has_pending_messages: false, is_idle: true });
    assert!(recorder.sent.lock().unwrap().is_empty());
}

#[test]
fn given_delivered_paths_when_marked_then_pending_is_rewritten_and_a_nudged_entry_is_appended() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), None, Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "first"), nudge("b.md", "second")]);
    delivery.mark_delivered("s1", &context, &["a.md".to_string()], KibitzerDeliveryVia::Steer);
    assert_eq!(pending_paths(root.path()), std::collections::BTreeSet::from(["b.md".to_string()]));
    let entries = recorder.entries.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["via"], "steer");
}

#[test]
fn given_a_held_set_when_drained_for_prompt_then_it_is_returned_and_retracted() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let delivery = delivery(root.path(), None, Arc::new(Recorder::default()));
    delivery.accept("s1", &context, &[nudge("a.md", "first")]);
    assert_eq!(delivery.drain_for_prompt("s1").len(), 1);
    assert!(delivery.drain_for_prompt("s1").is_empty());
}

#[test]
fn given_a_compaction_when_accepted_then_held_and_pending_are_retracted() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), Some(Arc::new(ReentrantCoordinator { recorder: Arc::clone(&recorder), root: root.path().to_path_buf() })), Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    delivery.on_compaction_accepted("s1", &context);
    assert!(recorder.removed.lock().unwrap().contains(&"kibitzer:a.md".to_string()));
    assert!(pending_paths(root.path()).is_empty());
    assert!(delivery.drain_for_prompt("s1").is_empty());
}

#[test]
fn given_a_session_shutdown_when_called_then_held_and_coordinator_are_retracted() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), Some(Arc::new(ReentrantCoordinator { recorder: Arc::clone(&recorder), root: root.path().to_path_buf() })), Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    delivery.on_session_shutdown("s1");
    assert!(recorder.removed.lock().unwrap().contains(&"kibitzer:a.md".to_string()));
    assert!(delivery.drain_for_prompt("s1").is_empty());
}

#[test]
fn given_a_tool_call_that_never_reports_when_the_turn_ends_then_it_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), None, Arc::clone(&recorder));
    delivery.mark_running("s1");
    delivery.mark_tool_started("s1", "call-1");
    delivery.mark_turn_ended("s1");
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    assert!(recorder.sent.lock().unwrap().is_empty());
}

/// A coordinator that REFUSES every enqueue (the retired-queue contract): it took NO ownership, so
/// the caller keeps the nudge and its pending handoff and must never read the refusal as delivered.
struct RefusingCoordinator { recorder: Arc<Recorder> }
impl KibitzerIdleCoordinator for RefusingCoordinator {
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool {
        self.recorder.enqueued.lock().unwrap().push(entry.key.clone());
        false
    }
    fn remove(&self, key: &str) { self.recorder.removed.lock().unwrap().push(key.to_string()); }
}

/// A refused enqueue is the upstream throw path: the accepted nudge and its pending handoff are
/// retained, the refusal is surfaced, and NO coordinator key is tracked - so a later drain returns
/// the retained nudge once and never retracts a key the coordinator never held.
#[test]
fn given_a_refusing_coordinator_when_accepting_then_the_nudge_survives_without_a_tracked_key() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let delivery = delivery(root.path(), Some(Arc::new(RefusingCoordinator { recorder: Arc::clone(&recorder) })), Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "hint")]);
    assert!(recorder.warns.lock().unwrap().iter().any(|message| message.contains("coordinator enqueue skipped")));
    assert_eq!(pending_paths(root.path()), std::collections::BTreeSet::from(["a.md".to_string()]));
    let drained: Vec<String> = delivery.drain_for_prompt("s1").into_iter().map(|nudge| nudge.path).collect();
    assert_eq!(drained, vec!["a.md".to_string()]);
    assert!(recorder.removed.lock().unwrap().is_empty());
    assert!(delivery.drain_for_prompt("s1").is_empty());
}

/// A coordinator that fires the entry's `on_flushed` SYNCHRONOUSLY from `enqueue` (an eager flush),
/// then reports success. `accept` must track a key only for a nudge that is STILL held, so the
/// callback's own delivery cannot be followed by a phantom key the coordinator no longer holds.
struct FlushOnEnqueueCoordinator { recorder: Arc<Recorder>, flush_key: String }
impl KibitzerIdleCoordinator for FlushOnEnqueueCoordinator {
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool {
        self.recorder.enqueued.lock().unwrap().push(entry.key.clone());
        if entry.key == self.flush_key { (entry.on_flushed)(); }
        true
    }
    fn remove(&self, key: &str) { self.recorder.removed.lock().unwrap().push(key.to_string()); }
}

/// Deterministic synchronous reentry (no sleeps, no threads): the callback delivers `a.md` before
/// `enqueue` returns, so `accept` must not re-add `kibitzer:a.md`; only `kibitzer:b.md` is retracted
/// by the later drain, proving no phantom key survived the callback.
#[test]
fn given_a_coordinator_that_flushes_synchronously_when_accepting_then_no_phantom_key_survives() {
    let root = tempfile::tempdir().unwrap();
    let context = context(root.path());
    let recorder = Arc::new(Recorder::default());
    let coordinator = Arc::new(FlushOnEnqueueCoordinator { recorder: Arc::clone(&recorder), flush_key: "kibitzer:a.md".to_string() });
    let delivery = delivery(root.path(), Some(coordinator), Arc::clone(&recorder));
    delivery.accept("s1", &context, &[nudge("a.md", "first"), nudge("b.md", "second")]);
    assert_eq!(pending_paths(root.path()), std::collections::BTreeSet::from(["b.md".to_string()]));
    assert_eq!(recorder.removed.lock().unwrap().clone(), vec!["kibitzer:a.md".to_string()]);
    let drained: Vec<String> = delivery.drain_for_prompt("s1").into_iter().map(|nudge| nudge.path).collect();
    assert_eq!(drained, vec!["b.md".to_string()]);
    assert_eq!(recorder.removed.lock().unwrap().clone(), vec!["kibitzer:a.md".to_string(), "kibitzer:b.md".to_string()]);
}
