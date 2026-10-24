//! Server-side execution barrier (senpi#3566, plan todo 4); port of `launch-enforcer.ts`. For every `launch` the server
//! records the containment handle durably, then acknowledges. The child must then deliver `launch_proof` with
//! `ackTick < invokeTick` from its shared clock within `proofMs` of the ack. A missing proof, a null `ackTick` or an
//! unordered pair is a `tool_child_barrier_violation`. `launch_done` has no deadline: a correctly gated command may run
//! as long as it likes.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde_json::json;
use tokio::sync::Notify;

use crate::barrier_clock::proof_is_ordered;
use crate::descendants::{DescendantEntry, DescendantsLedger, LaunchLog};
use crate::protocol::{ChildToServer, LaunchContainment};

pub const LAUNCH_PROOF_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierViolationReason {
    ProofMissing,
    ProofUnacked,
    ProofUnordered,
    ProofUnknownLaunch,
    LedgerUnwritable,
    BreakawayContradictsContainment,
}

impl BarrierViolationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProofMissing => "proof_missing",
            Self::ProofUnacked => "proof_unacked",
            Self::ProofUnordered => "proof_unordered",
            Self::ProofUnknownLaunch => "proof_unknown_launch",
            Self::LedgerUnwritable => "ledger_unwritable",
            Self::BreakawayContradictsContainment => "breakaway_contradicts_containment",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierViolationCode {
    ToolChildBarrierViolation,
    /// `tool_child_containment_violation` when the started command is not where its launch said it would be.
    ToolChildContainmentViolation,
}

impl BarrierViolationCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolChildBarrierViolation => "tool_child_barrier_violation",
            Self::ToolChildContainmentViolation => "tool_child_containment_violation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarrierViolation {
    pub launch_id: String,
    pub job_id: Option<String>,
    pub reason: BarrierViolationReason,
    pub code: BarrierViolationCode,
}

/// A started timer's cancel handle.
pub type CancelTimer = Box<dyn FnOnce() + Send>;
pub type SetTimer = Arc<dyn Fn(Box<dyn FnOnce() + Send>, u64) -> CancelTimer + Send + Sync>;

/// Test-only gate that resolves when the test releases a launch's ack.
pub type HoldAckFn = dyn Fn(String) -> BoxFuture<'static, io::Result<()>> + Send + Sync;

#[derive(Clone)]
pub struct LaunchEnforcerOptions {
    pub generation: u64,
    pub ledger: Arc<DescendantsLedger>,
    pub log: Arc<LaunchLog>,
    pub send_ack: Arc<dyn Fn(&str) + Send + Sync>,
    pub on_violation: Arc<dyn Fn(BarrierViolation) + Send + Sync>,
    pub leader_start: Arc<dyn Fn(i64) -> BoxFuture<'static, Option<String>> + Send + Sync>,
    /// Test-only gate: resolves when the test releases this launch's ack (`HOLD_LAUNCH_ACK`).
    pub hold_ack: Option<Arc<HoldAckFn>>,
    pub set_timer: Option<SetTimer>,
    pub proof_ms: Option<u64>,
    /// Windows: a live job listing, recorded as confirmation of creation-time membership (never as a refutation).
    pub confirm_member: Option<Arc<dyn Fn(i64) -> bool + Send + Sync>>,
}

struct Inflight {
    job_id: String,
    containment: LaunchContainment,
    acked: bool,
    cancel_timer: Option<CancelTimer>,
    proven: bool,
}

struct Shared {
    options: LaunchEnforcerOptions,
    inflight: Mutex<HashMap<String, Arc<Mutex<Inflight>>>>,
    pending_recordings: AtomicUsize,
    recordings_settled: Notify,
    violated: Mutex<bool>,
}

#[derive(Clone)]
pub struct LaunchEnforcer {
    shared: Arc<Shared>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn log_frame(log: &LaunchLog, frame: &ChildToServer) {
    if let Ok(value) = serde_json::to_value(frame) {
        drop(log.record(value));
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

impl LaunchEnforcer {
    pub fn new(options: LaunchEnforcerOptions) -> Self {
        Self {
            shared: Arc::new(Shared {
                options,
                inflight: Mutex::new(HashMap::new()),
                pending_recordings: AtomicUsize::new(0),
                recordings_settled: Notify::new(),
                violated: Mutex::new(false),
            }),
        }
    }

    /// `frame` is a `launch` frame; any other frame is ignored. A held ack that fails (its control directory is
    /// missing) surfaces here, as it does upstream.
    pub async fn on_launch(&self, frame: &ChildToServer) -> io::Result<()> {
        let ChildToServer::Launch {
            launch_id,
            job_id,
            containment,
            ..
        } = frame
        else {
            return Ok(());
        };
        let shared = &self.shared;
        let inflight = Arc::new(Mutex::new(Inflight {
            job_id: job_id.clone(),
            containment: *containment,
            acked: false,
            cancel_timer: None,
            proven: false,
        }));
        lock(&shared.inflight).insert(launch_id.clone(), Arc::clone(&inflight));
        shared.pending_recordings.fetch_add(1, Ordering::SeqCst);
        let recorded = self.record(frame).await;
        if shared.pending_recordings.fetch_sub(1, Ordering::SeqCst) == 1 {
            shared.recordings_settled.notify_waiters();
        }
        if recorded.is_err() {
            // No ledger entry means no containment record: the launch is never acked, and the child cannot be trusted
            // to stay inside the barrier, so it ends like any other violation (never an unhandled rejection).
            self.violate(launch_id, BarrierViolationReason::LedgerUnwritable, None);
            return Ok(());
        }
        if let Some(hold_ack) = &shared.options.hold_ack {
            hold_ack(launch_id.clone()).await?;
        }
        if *lock(&shared.violated) || lock(&inflight).proven {
            return Ok(());
        }
        lock(&inflight).acked = true;
        (shared.options.send_ack)(launch_id);
        // Armed before any await: the proof can arrive while the ack is being logged, and a timer armed after that would
        // still fire (proof_missing) for a launch already proven.
        if !lock(&inflight).proven {
            let this = self.clone();
            let id = launch_id.clone();
            let fire: Box<dyn FnOnce() + Send> = Box::new(move || {
                this.violate(&id, BarrierViolationReason::ProofMissing, None);
            });
            let ms = shared.options.proof_ms.unwrap_or(LAUNCH_PROOF_MS);
            let cancel = match &shared.options.set_timer {
                Some(set_timer) => set_timer(fire, ms),
                None => default_timer(fire, ms),
            };
            let mut state = lock(&inflight);
            if state.proven {
                drop(state);
                cancel();
            } else {
                state.cancel_timer = Some(cancel);
            }
        }
        let _ = shared
            .options
            .log
            .record(json!({"type": "launch_ack", "launchId": launch_id}))
            .await;
        Ok(())
    }

    /// Resolves once every launch received so far is in the ledger. Teardown reads the ledger after this: a launch whose
    /// entry was still being written when the child was killed (a violation kills at once) is swept like any other.
    pub async fn recorded(&self) {
        loop {
            let settled = self.shared.recordings_settled.notified();
            if self.shared.pending_recordings.load(Ordering::SeqCst) == 0 {
                return;
            }
            settled.await;
        }
    }

    async fn record(&self, frame: &ChildToServer) -> io::Result<()> {
        let ChildToServer::Launch {
            launch_id,
            origin,
            api,
            containment,
            lifetime,
            pid,
            pgid,
            ..
        } = frame
        else {
            return Ok(());
        };
        let options = &self.shared.options;
        let leader_start = match pgid {
            None => None,
            Some(pgid) => (options.leader_start)(*pgid).await,
        };
        let entry = DescendantEntry {
            launch_id: launch_id.clone(),
            generation: options.generation,
            origin: *origin,
            api: *api,
            containment: *containment,
            lifetime: *lifetime,
            pid: *pid,
            pgid: *pgid,
            leader_start,
            at_ms: now_ms(),
        };
        // The ledger is the containment record and gates the ack; the diagnostic log is written beside it, off that path.
        log_frame(&options.log, frame);
        options.ledger.append(entry).await
    }

    /// `frame` is a `launch_proof` frame; any other frame is ignored.
    pub fn on_proof(&self, frame: &ChildToServer) {
        let ChildToServer::LaunchProof {
            launch_id,
            ack_tick,
            invoke_tick,
        } = frame
        else {
            return;
        };
        let options = &self.shared.options;
        log_frame(&options.log, frame);
        let inflight = lock(&self.shared.inflight).get(launch_id).cloned();
        let Some(inflight) = inflight else {
            self.violate(launch_id, BarrierViolationReason::ProofUnknownLaunch, None);
            return;
        };
        let (cancel, acked) = {
            let mut state = lock(&inflight);
            state.proven = true;
            (state.cancel_timer.take(), state.acked)
        };
        if let Some(cancel) = cancel {
            cancel();
        }
        if ack_tick.is_none() || !acked {
            self.violate(launch_id, BarrierViolationReason::ProofUnacked, None);
        } else if !proof_is_ordered(*ack_tick, *invoke_tick) {
            self.violate(launch_id, BarrierViolationReason::ProofUnordered, None);
        }
    }

    /// Windows creation-time evidence: a command started by a creator inside the job is a member from creation (job
    /// membership is inherited), unless it broke away. A `breakaway` that contradicts the ledgered containment means the
    /// command is not where the server recorded it, so the generation is ended as a containment violation.
    pub fn on_command_started(&self, frame: &ChildToServer) {
        let ChildToServer::CommandStarted {
            launch_id,
            pid,
            creator_in_job,
            breakaway,
            ..
        } = frame
        else {
            return;
        };
        let options = &self.shared.options;
        log_frame(&options.log, frame);
        let inflight = lock(&self.shared.inflight).get(launch_id).cloned();
        let Some(inflight) = inflight else {
            return;
        };
        let containment = lock(&inflight).containment;
        if *breakaway != (containment == LaunchContainment::Breakaway) {
            self.violate(
                launch_id,
                BarrierViolationReason::BreakawayContradictsContainment,
                Some(BarrierViolationCode::ToolChildContainmentViolation),
            );
            return;
        }
        let member = *creator_in_job && !*breakaway;
        let mut record = json!({
            "type": "job_membership",
            "launchId": launch_id,
            "pid": pid,
            "member": member,
            "evidence": "creation",
        });
        if member && let Some(confirm) = &options.confirm_member {
            record["confirmed"] = json!(confirm(*pid));
        }
        drop(options.log.record(record));
    }

    /// `frame` is a `launch_done` frame; any other frame is ignored.
    pub fn on_done(&self, frame: &ChildToServer) {
        if matches!(frame, ChildToServer::LaunchDone { .. }) {
            log_frame(&self.shared.options.log, frame);
        }
    }

    pub fn dispose(&self) {
        let drained: Vec<Arc<Mutex<Inflight>>> = lock(&self.shared.inflight)
            .drain()
            .map(|(_, v)| v)
            .collect();
        for inflight in drained {
            let cancel = lock(&inflight).cancel_timer.take();
            if let Some(cancel) = cancel {
                cancel();
            }
        }
    }

    fn violate(
        &self,
        launch_id: &str,
        reason: BarrierViolationReason,
        code: Option<BarrierViolationCode>,
    ) {
        let code = code.unwrap_or(BarrierViolationCode::ToolChildBarrierViolation);
        let job_id = lock(&self.shared.inflight)
            .get(launch_id)
            .map(|inflight| lock(inflight).job_id.clone());
        drop(self.shared.options.log.record(json!({
            "type": "violation",
            "code": code.as_str(),
            "launchId": launch_id,
            "reason": reason.as_str(),
        })));
        {
            let mut violated = lock(&self.shared.violated);
            if *violated {
                return;
            }
            *violated = true;
        }
        self.dispose();
        (self.shared.options.on_violation)(BarrierViolation {
            launch_id: launch_id.to_owned(),
            job_id,
            reason,
            code,
        });
    }
}

fn default_timer(fire: Box<dyn FnOnce() + Send>, ms: u64) -> CancelTimer {
    let task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        fire();
    });
    Box::new(move || task.abort())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LaunchApi, LaunchLifetime, LaunchOrigin};
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use tokio::sync::oneshot;

    const START: &str = "Sat Oct 18 05:00:00 2026";

    pub(super) struct FakeTimer {
        fire: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        ms: u64,
        cancelled: Arc<AtomicBool>,
    }

    impl FakeTimer {
        fn cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
        fn fire(&self) {
            let fire = lock(&self.fire).take();
            if let Some(fire) = fire {
                fire();
            }
        }
    }

    struct Rig {
        _root: tempfile::TempDir,
        ledger: Arc<DescendantsLedger>,
        log: Arc<LaunchLog>,
        enforcer: LaunchEnforcer,
        acks: Arc<Mutex<Vec<String>>>,
        violations: Arc<Mutex<Vec<BarrierViolation>>>,
        timers: Arc<Mutex<Vec<Arc<FakeTimer>>>>,
    }

    #[derive(Default)]
    struct Extra {
        hold: Option<Arc<HoldAckFn>>,
        on_ack: Option<Arc<crate::client::CancelAckFn>>,
        ledger_path: Option<std::path::PathBuf>,
        log_path: Option<std::path::PathBuf>,
        confirm: Option<Arc<dyn Fn(i64) -> bool + Send + Sync>>,
        leader_start: Option<Option<String>>,
    }

    fn rig(extra: Extra) -> Rig {
        let root = tempfile::tempdir().unwrap();
        let acks = Arc::new(Mutex::new(Vec::new()));
        let violations = Arc::new(Mutex::new(Vec::new()));
        let timers: Arc<Mutex<Vec<Arc<FakeTimer>>>> = Arc::new(Mutex::new(Vec::new()));
        let ledger = Arc::new(DescendantsLedger::new(
            extra
                .ledger_path
                .unwrap_or_else(|| root.path().join("descendants.ledger")),
        ));
        let log = Arc::new(LaunchLog::new(
            extra
                .log_path
                .unwrap_or_else(|| root.path().join("launches.jsonl")),
        ));
        let on_ack = extra.on_ack;
        let ack_sink = Arc::clone(&acks);
        let violation_sink = Arc::clone(&violations);
        let timer_sink = Arc::clone(&timers);
        let leader = extra.leader_start.unwrap_or_else(|| Some(START.to_owned()));
        let enforcer = LaunchEnforcer::new(LaunchEnforcerOptions {
            generation: 1,
            ledger: Arc::clone(&ledger),
            log: Arc::clone(&log),
            send_ack: Arc::new(move |id| {
                lock(&ack_sink).push(id.to_owned());
                if let Some(on_ack) = &on_ack {
                    on_ack(id);
                }
            }),
            on_violation: Arc::new(move |violation| lock(&violation_sink).push(violation)),
            leader_start: Arc::new(move |_| {
                let leader = leader.clone();
                Box::pin(async move { leader })
            }),
            hold_ack: extra.hold,
            set_timer: Some(Arc::new(move |fire, ms| {
                let cancelled = Arc::new(AtomicBool::new(false));
                lock(&timer_sink).push(Arc::new(FakeTimer {
                    fire: Mutex::new(Some(fire)),
                    ms,
                    cancelled: Arc::clone(&cancelled),
                }));
                Box::new(move || cancelled.store(true, Ordering::SeqCst))
            })),
            proof_ms: None,
            confirm_member: extra.confirm,
        });
        Rig {
            _root: root,
            ledger,
            log,
            enforcer,
            acks,
            violations,
            timers,
        }
    }

    fn launch_with(
        launch_id: &str,
        origin: LaunchOrigin,
        api: LaunchApi,
        containment: LaunchContainment,
        lifetime: LaunchLifetime,
        pid: Option<i64>,
    ) -> ChildToServer {
        ChildToServer::Launch {
            launch_id: launch_id.to_owned(),
            job_id: "j1".to_owned(),
            origin,
            api,
            containment,
            lifetime,
            pid,
            pgid: pid,
        }
    }

    fn launch(launch_id: &str) -> ChildToServer {
        launch_with(
            launch_id,
            LaunchOrigin::Tool,
            LaunchApi::BunSpawn,
            LaunchContainment::OwnGroup,
            LaunchLifetime::Owned,
            Some(4242),
        )
    }

    fn proof(launch_id: &str, ack_tick: Option<u64>, invoke_tick: u64) -> ChildToServer {
        ChildToServer::LaunchProof {
            launch_id: launch_id.to_owned(),
            ack_tick,
            invoke_tick,
        }
    }

    fn violation(
        launch_id: &str,
        job_id: Option<&str>,
        reason: BarrierViolationReason,
    ) -> BarrierViolation {
        BarrierViolation {
            launch_id: launch_id.to_owned(),
            job_id: job_id.map(str::to_owned),
            reason,
            code: BarrierViolationCode::ToolChildBarrierViolation,
        }
    }

    async fn records(rig: &Rig) -> Vec<serde_json::Value> {
        rig.log.settled().await;
        tokio::fs::read_to_string(rig.log.path())
            .await
            .unwrap()
            .trim()
            .split('\n')
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn never_acks_a_launch_whose_ledger_entry_cannot_be_written_reports_a_violation_and_never_fails()
     {
        let blocked = tempfile::tempdir().unwrap();
        let not_a_dir = blocked.path().join("file");
        std::fs::write(&not_a_dir, "").unwrap();
        let rig = rig(Extra {
            ledger_path: Some(not_a_dir.join("descendants.ledger")),
            ..Extra::default()
        });
        assert!(rig.enforcer.on_launch(&launch("L1")).await.is_ok());
        assert!(lock(&rig.acks).is_empty());
        assert_eq!(
            *lock(&rig.violations),
            vec![violation(
                "L1",
                Some("j1"),
                BarrierViolationReason::LedgerUnwritable
            )]
        );
    }

    #[tokio::test]
    async fn acks_a_recorded_launch_even_when_the_diagnostic_log_cannot_be_written() {
        let blocked = tempfile::tempdir().unwrap();
        let not_a_dir = blocked.path().join("file");
        std::fs::write(&not_a_dir, "").unwrap();
        let rig = rig(Extra {
            log_path: Some(not_a_dir.join("launches.jsonl")),
            ..Extra::default()
        });
        rig.enforcer.on_launch(&launch("L1")).await.unwrap();
        assert_eq!(*lock(&rig.acks), vec!["L1".to_owned()]);
        assert!(lock(&rig.violations).is_empty());
        rig.log.settled().await;
        assert!(rig.log.failed_writes() > 0);
    }

    #[tokio::test]
    async fn never_reports_proof_missing_for_a_proof_that_arrived_while_the_ack_was_being_logged() {
        let enforcer_ref: Arc<OnceLock<LaunchEnforcer>> = Arc::new(OnceLock::new());
        let hook = Arc::clone(&enforcer_ref);
        let rigged = rig(Extra {
            on_ack: Some(Arc::new(move |id| {
                if let Some(enforcer) = hook.get() {
                    enforcer.on_proof(&proof(id, Some(1), 2));
                }
            })),
            ..Extra::default()
        });
        assert!(enforcer_ref.set(rigged.enforcer.clone()).is_ok());
        rigged.enforcer.on_launch(&launch("L1")).await.unwrap();
        for timer in lock(&rigged.timers).iter() {
            if !timer.cancelled() {
                timer.fire();
            }
        }
        assert!(lock(&rigged.violations).is_empty());
    }

    #[tokio::test]
    async fn persists_the_containment_handle_before_acknowledging_and_accepts_an_ordered_proof() {
        let rig = rig(Extra::default());
        rig.enforcer.on_launch(&launch("l1")).await.unwrap();
        let persisted = DescendantsLedger::read_persisted(rig.ledger.path()).await;
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].launch_id, "l1");
        assert_eq!(persisted[0].pgid, Some(4242));
        assert_eq!(persisted[0].lifetime, LaunchLifetime::Owned);
        assert_eq!(persisted[0].leader_start.as_deref(), Some(START));
        assert_eq!(*lock(&rig.acks), vec!["l1".to_owned()]);
        rig.enforcer.on_proof(&proof("l1", Some(3), 4));
        assert!(lock(&rig.timers)[0].cancelled());
        assert!(lock(&rig.violations).is_empty());
        let types: Vec<String> = records(&rig)
            .await
            .iter()
            .map(|r| r["type"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(types, vec!["launch", "launch_ack", "launch_proof"]);
    }

    #[tokio::test]
    async fn raises_a_violation_for_each_bad_proof() {
        let cases = [
            (
                "a null ackTick",
                None,
                4,
                BarrierViolationReason::ProofUnacked,
            ),
            (
                "an invocation before the ack",
                Some(5),
                4,
                BarrierViolationReason::ProofUnordered,
            ),
            (
                "equal ticks",
                Some(4),
                4,
                BarrierViolationReason::ProofUnordered,
            ),
        ];
        for (label, ack_tick, invoke_tick, reason) in cases {
            let rig = rig(Extra::default());
            rig.enforcer.on_launch(&launch("l1")).await.unwrap();
            rig.enforcer.on_proof(&proof("l1", ack_tick, invoke_tick));
            assert_eq!(
                *lock(&rig.violations),
                vec![violation("l1", Some("j1"), reason)],
                "{label}"
            );
        }
    }

    #[tokio::test]
    async fn raises_proof_missing_when_the_proof_deadline_passes_after_the_ack() {
        let rig = rig(Extra::default());
        rig.enforcer.on_launch(&launch("l1")).await.unwrap();
        let ms: Vec<u64> = lock(&rig.timers).iter().map(|t| t.ms).collect();
        assert_eq!(ms, vec![5_000]);
        let timer = Arc::clone(&lock(&rig.timers)[0]);
        timer.fire();
        assert_eq!(
            *lock(&rig.violations),
            vec![violation(
                "l1",
                Some("j1"),
                BarrierViolationReason::ProofMissing
            )]
        );
    }

    #[tokio::test]
    async fn detects_an_invocation_while_the_ack_is_still_held_without_ever_acknowledging() {
        let (release, released) = oneshot::channel::<()>();
        let (reached_tx, reached) = oneshot::channel::<()>();
        let released = Arc::new(Mutex::new(Some(released)));
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        let rig = rig(Extra {
            hold: Some(Arc::new(move |_| {
                if let Some(tx) = lock(&reached_tx).take() {
                    let _ = tx.send(());
                }
                let wait = lock(&released).take();
                Box::pin(async move {
                    if let Some(wait) = wait {
                        let _ = wait.await;
                    }
                    Ok(())
                })
            })),
            ..Extra::default()
        });
        let enforcer = rig.enforcer.clone();
        let frame = launch("l1");
        let pending = tokio::spawn(async move { enforcer.on_launch(&frame).await });
        tokio::time::timeout(Duration::from_secs(10), reached)
            .await
            .unwrap()
            .unwrap();
        rig.enforcer.on_proof(&proof("l1", None, 2));
        assert_eq!(
            *lock(&rig.violations),
            vec![violation(
                "l1",
                Some("j1"),
                BarrierViolationReason::ProofUnacked
            )]
        );
        let _ = release.send(());
        pending.await.unwrap().unwrap();
        assert!(lock(&rig.acks).is_empty());
    }

    #[tokio::test]
    async fn treats_a_proof_for_an_unknown_launch_as_a_violation_and_reports_only_the_first() {
        let rig = rig(Extra::default());
        rig.enforcer.on_proof(&proof("ghost", Some(1), 2));
        rig.enforcer.on_proof(&proof("ghost2", Some(1), 2));
        assert_eq!(
            *lock(&rig.violations),
            vec![violation(
                "ghost",
                None,
                BarrierViolationReason::ProofUnknownLaunch
            )]
        );
    }

    #[tokio::test]
    async fn sets_no_deadline_on_launch_done() {
        let rig = rig(Extra::default());
        rig.enforcer.on_launch(&launch("l1")).await.unwrap();
        rig.enforcer.on_proof(&proof("l1", Some(1), 2));
        rig.enforcer.on_done(&ChildToServer::LaunchDone {
            launch_id: "l1".to_owned(),
            exit_code: Some(0),
            signal: None,
        });
        assert!(lock(&rig.timers).iter().all(|t| t.cancelled()));
        assert!(lock(&rig.violations).is_empty());
    }

    // ---- launch-enforcer-membership.test.ts ----

    fn membership_rig(confirm: Option<Arc<dyn Fn(i64) -> bool + Send + Sync>>) -> Rig {
        rig(Extra {
            confirm,
            leader_start: Some(None),
            ..Extra::default()
        })
    }

    fn membership_launch(launch_id: &str, containment: LaunchContainment) -> ChildToServer {
        launch_with(
            launch_id,
            LaunchOrigin::Kernel,
            LaunchApi::CpSpawn,
            containment,
            if containment == LaunchContainment::Breakaway {
                LaunchLifetime::Detached
            } else {
                LaunchLifetime::Owned
            },
            None,
        )
    }

    fn started(launch_id: &str, creator_in_job: bool, breakaway: bool) -> ChildToServer {
        ChildToServer::CommandStarted {
            launch_id: launch_id.to_owned(),
            pid: 7000,
            started_at_ms: 1,
            creator_in_job,
            breakaway,
        }
    }

    #[tokio::test]
    async fn records_creation_time_membership_for_a_job_launch_and_non_membership_for_a_breakaway_one()
     {
        let rig = membership_rig(None);
        rig.enforcer
            .on_launch(&membership_launch("in-job", LaunchContainment::Job))
            .await
            .unwrap();
        rig.enforcer
            .on_launch(&membership_launch("away", LaunchContainment::Breakaway))
            .await
            .unwrap();
        rig.enforcer
            .on_command_started(&started("in-job", true, false));
        rig.enforcer
            .on_command_started(&started("away", true, true));
        let memberships: Vec<serde_json::Value> = records(&rig)
            .await
            .into_iter()
            .filter(|r| r["type"] == "job_membership")
            .collect();
        assert_eq!(memberships.len(), 2);
        assert_eq!(memberships[0]["launchId"], "in-job");
        assert_eq!(memberships[0]["member"], true);
        assert_eq!(memberships[0]["evidence"], "creation");
        assert_eq!(memberships[1]["launchId"], "away");
        assert_eq!(memberships[1]["member"], false);
        assert_eq!(memberships[1]["evidence"], "creation");
        assert!(lock(&rig.violations).is_empty());
    }

    #[tokio::test]
    async fn adds_a_live_listing_as_confirmation_for_a_member_and_records_a_missing_listing_without_refuting_it()
     {
        for listed in [true, false] {
            let rig = membership_rig(Some(Arc::new(move |_| listed)));
            rig.enforcer
                .on_launch(&membership_launch("l", LaunchContainment::Job))
                .await
                .unwrap();
            rig.enforcer.on_command_started(&started("l", true, false));
            let all = records(&rig).await;
            let record = all.iter().find(|r| r["type"] == "job_membership").unwrap();
            assert_eq!(record["member"], true);
            assert_eq!(record["confirmed"], listed);
            assert!(lock(&rig.violations).is_empty());
        }
    }

    #[tokio::test]
    async fn never_claims_membership_when_the_creator_itself_was_outside_the_job() {
        let rig = membership_rig(None);
        rig.enforcer
            .on_launch(&membership_launch("l", LaunchContainment::Job))
            .await
            .unwrap();
        rig.enforcer.on_command_started(&started("l", false, false));
        let all = records(&rig).await;
        let record = all.iter().find(|r| r["type"] == "job_membership").unwrap();
        assert_eq!(record["member"], false);
    }

    #[tokio::test]
    async fn raises_tool_child_containment_violation_when_breakaway_contradicts_the_ledgered_containment_both_ways()
     {
        for (containment, breakaway) in [
            (LaunchContainment::Job, true),
            (LaunchContainment::Breakaway, false),
        ] {
            let rig = membership_rig(None);
            rig.enforcer
                .on_launch(&membership_launch("l", containment))
                .await
                .unwrap();
            rig.enforcer
                .on_command_started(&started("l", true, breakaway));
            assert_eq!(
                *lock(&rig.violations),
                vec![BarrierViolation {
                    launch_id: "l".to_owned(),
                    job_id: Some("j1".to_owned()),
                    reason: BarrierViolationReason::BreakawayContradictsContainment,
                    code: BarrierViolationCode::ToolChildContainmentViolation,
                }]
            );
            assert!(
                !records(&rig)
                    .await
                    .iter()
                    .any(|r| r["type"] == "job_membership")
            );
        }
    }
}
