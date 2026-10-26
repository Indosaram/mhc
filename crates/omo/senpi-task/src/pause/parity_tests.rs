use super::{admission_barrier::*, guidance::*, ports::*, registry::*, run_controller::*};
use std::sync::{Arc, Mutex};

fn participant(key: &str, sequence: u64, phase: AdmissionPhase) -> PauseParticipant {
    PauseParticipant {
        key: key.into(),
        kind: if phase == AdmissionPhase::Parked {
            ParticipantKind::Task
        } else {
            ParticipantKind::HeldAdmission
        },
        run_ref: RunRef::task(key),
        task_id: Some(key.into()),
        node_id: None,
        run_epoch: Some(0),
        admission: Some(ParticipantAdmission {
            model: "test/model".into(),
            lane_key: "test/model".into(),
            sequence,
            phase,
        }),
        state: ParticipantState::Pending,
        ack_seq: None,
        observed_child_seq: None,
        resume_turn_id: None,
        lost_reason: None,
        at: "1970-01-01".into(),
    }
}
fn admission() -> AdmissionParticipant {
    AdmissionParticipant {
        key: "a".into(),
        run_ref: RunRef::task("a"),
    }
}
#[test]
fn credentials_are_identity_bound_and_resuming_rejects_wrong_sequence() {
    let barrier = AdmissionBarrier::new();
    barrier.close("root", 1);
    let old = barrier.resume("root", 1, "resume").expect("credential");
    assert!(barrier.resume("root", 2, "resume").is_err());
    barrier.restore(
        "root",
        AdmissionState::Resuming {
            pause_seq: 1,
            resume_id: "resume".into(),
        },
        1,
        false,
    );
    let new = barrier
        .resume("root", 1, "resume")
        .expect("restored credential");
    assert_ne!(old, new);
    assert!(!barrier.release("root", &old));
    assert!(matches!(
        barrier.enter("root", admission(), Some(old)),
        AdmissionEntry::Held { .. }
    ));
    let AdmissionEntry::Entered { ticket } = barrier.enter("root", admission(), Some(new)) else {
        panic!("entered")
    };
    barrier.close("root", 2);
    let latest = barrier.resume("root", 2, "resume-2").expect("latest");
    assert!(matches!(
        ticket.check(),
        AdmissionCheck::Held { pause_seq: 2 }
    ));
    assert!(barrier.release("root", &latest));
    ticket.leave();
}
#[test]
fn registry_persists_restores_orders_and_freezes_cutoffs() {
    let dir = tempfile::tempdir().expect("dir");
    let barrier = AdmissionBarrier::new();
    let registry = PauseRegistry::new(dir.path(), barrier.clone());
    registry
        .begin(
            "root",
            1,
            vec![
                participant("queued", 0, AdmissionPhase::PreGrant),
                participant("later", 3, AdmissionPhase::Parked),
                participant("first", 1, AdmissionPhase::Parked),
            ],
        )
        .expect("begin");
    assert_eq!(
        barrier.state("root"),
        AdmissionState::Closed { pause_seq: 1 }
    );
    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("pause/root.json")).expect("saved"),
    )
    .expect("json");
    assert_eq!(value["parentSessionId"], "root");
    let restored = PauseRegistry::new(dir.path(), AdmissionBarrier::new());
    restored
        .restore(
            "root",
            RestoreLedger {
                pause_seq: 1,
                superseded: false,
                requested_resume_id: None,
            },
            Vec::new,
        )
        .expect("restore");
    assert!(matches!(
        restored.barrier.enter("root", admission(), None),
        AdmissionEntry::Held { .. }
    ));
    assert_eq!(
        restored
            .ordered_admissions("root")
            .expect("order")
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>(),
        ["first", "later", "queued"]
    );
    restored
        .update("root", 1, "first", |p| {
            p.state = ParticipantState::Held;
            p.ack_seq = Some(1);
            p.observed_child_seq = Some(8);
        })
        .expect("ack");
    let intent = restored
        .request_resume("root", 1, "resume")
        .expect("intent");
    assert_eq!(intent.child_seq_cutoffs["first"], 8);
    restored
        .update("root", 1, "first", |p| p.observed_child_seq = Some(9))
        .expect("observe");
    assert_eq!(
        restored
            .request_resume("root", 1, "resume")
            .expect("same intent")
            .child_seq_cutoffs["first"],
        8
    );
    restored
        .restore(
            "root",
            RestoreLedger {
                pause_seq: 2,
                superseded: false,
                requested_resume_id: None,
            },
            || vec![participant("first", 1, AdmissionPhase::Parked)],
        )
        .expect("rebuild");
    assert!(
        !restored
            .update("root", 1, "first", |p| p.state = ParticipantState::Resumed)
            .expect("stale")
    );
    assert_eq!(
        restored.require("root").expect("record").participants["first"].state,
        ParticipantState::Pending
    );
    for id in ["", ".", "..", "a/b", "a:b", "a\\b", "a\0b"] {
        assert!(restored.load(id).is_err());
    }
}
#[derive(Default)]
struct GuidanceHarness {
    saved: Vec<Guidance>,
    deliveries: Vec<HoldInput>,
    fail_save_after_delivery: bool,
}
impl GuidancePorts for GuidanceHarness {
    type Error = &'static str;
    fn save(&mut self, records: &[Guidance]) -> Result<(), Self::Error> {
        if self.fail_save_after_delivery && !self.deliveries.is_empty() {
            return Err("crash");
        }
        self.saved = records.to_vec();
        Ok(())
    }
    fn deliver(&mut self, _: &str, input: HoldInput) -> Result<Option<String>, Self::Error> {
        assert_eq!(self.saved[0].state, GuidanceState::Dispatching);
        self.deliveries.push(input);
        Ok(Some("entry".into()))
    }
}
fn draft() -> Vec<Guidance> {
    add_guidance(
        &[],
        "g".into(),
        GuidanceScope::Run,
        "before".into(),
        "0".into(),
    )
    .expect("draft")
}
#[test]
fn guidance_edits_removals_and_freezing() {
    let original = draft();
    let edited =
        change_guidance(&original, "g", Some("after".into()), false, "1".into()).expect("edit");
    assert_eq!(original[0].text, "before");
    assert_eq!(edited[0].revision, 1);
    assert_eq!(
        change_guidance(&edited, "g", None, true, "2".into()).expect("remove")[0].state,
        GuidanceState::Removed
    );
    assert_eq!(
        add_guidance(
            &original,
            "g".into(),
            GuidanceScope::Run,
            "x".into(),
            "0".into()
        ),
        Err(GuidanceError::DuplicateId)
    );
    let mut ports = GuidanceHarness::default();
    dispatch_guidance(
        &edited,
        &[GuidanceTarget {
            node_id: "n".into(),
            task_id: Some("t".into()),
        }],
        &mut ports,
    )
    .expect("dispatch");
    assert_eq!(
        change_guidance(&ports.saved, "g", None, true, "3".into()),
        Err(GuidanceError::Frozen)
    );
}
#[test]
fn guidance_crash_redelivers_stable_input_and_queued_targets_retain_custody() {
    let targets = [
        GuidanceTarget {
            node_id: "n".into(),
            task_id: Some("t".into()),
        },
        GuidanceTarget {
            node_id: "queued".into(),
            task_id: None,
        },
    ];
    let mut ports = GuidanceHarness {
        fail_save_after_delivery: true,
        ..Default::default()
    };
    assert!(dispatch_guidance(&draft(), &targets, &mut ports).is_err());
    let crash_snapshot = ports.saved.clone();
    ports.fail_save_after_delivery = false;
    dispatch_guidance(&crash_snapshot, &targets, &mut ports).expect("recover");
    assert_eq!(ports.deliveries.len(), 2);
    assert_eq!(ports.deliveries[0], ports.deliveries[1]);
    assert_eq!(ports.saved[0].state, GuidanceState::Dispatching);
    assert_eq!(ports.saved[0].deliveries.len(), 1);
    let acknowledged = ports.saved.clone();
    dispatch_guidance(&acknowledged, &targets, &mut ports).expect("idempotent");
    assert_eq!(ports.deliveries.len(), 2);
    let ready = [
        GuidanceTarget {
            node_id: "n".into(),
            task_id: Some("t".into()),
        },
        GuidanceTarget {
            node_id: "queued".into(),
            task_id: Some("q".into()),
        },
    ];
    dispatch_guidance(&acknowledged, &ready, &mut ports).expect("queued starts");
    assert_eq!(ports.saved[0].state, GuidanceState::Delivered);
}
struct Effects {
    registry: PauseRegistry,
    events: Mutex<Vec<&'static str>>,
    newer_hold: bool,
    guidance_entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    guidance_release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl Effects {
    fn event(&self, event: &'static str) {
        self.events.lock().expect("events").push(event);
    }
}
impl RunPauseEffects for Effects {
    fn snapshot(&self, _: &str, _: &[AdmissionParticipant]) -> Vec<PauseParticipant> {
        vec![participant("a", 1, AdmissionPhase::Parked)]
    }
    fn quiesce<'a>(&'a self, parent: &'a str, seq: u64) -> Effect<'a, ()> {
        assert_eq!(
            self.registry.barrier.state(parent),
            AdmissionState::Closed { pause_seq: seq }
        );
        assert_eq!(
            self.registry.require(parent).expect("ledger").participants["a"].state,
            ParticipantState::Pending
        );
        self.event("quiesce");
        Box::pin(async { Ok(()) })
    }
    fn add_fences<'a>(&'a self, _: &'a str, _: u64, _: Vec<RunRef>) -> Effect<'a, ()> {
        self.event("fences");
        Box::pin(async { Ok(()) })
    }
    fn pause<'a>(
        &'a self,
        _: &'a PauseParticipant,
        _: RunPauseOwner,
    ) -> Effect<'a, PauseParticipantResult> {
        self.event("pause");
        Box::pin(async { Ok(PauseParticipantResult::Held { child_seq: 8 }) })
    }
    fn dispatch_guidance<'a>(&'a self, _: &'a str, _: u64, _: &'a str) -> Effect<'a, ()> {
        self.event("guidance");
        let entered = self.guidance_entered.lock().expect("entered").take();
        let release = self.guidance_release.lock().expect("release").take();
        Box::pin(async move {
            if let Some(entered) = entered {
                entered.send(()).expect("signal");
            }
            if let Some(release) = release {
                release.await.expect("release");
            }
            Ok(())
        })
    }
    fn resume<'a>(
        &'a self,
        _: &'a PauseParticipant,
        request: ResumeParticipantRequest,
        _: Option<ResumeCredential>,
    ) -> Effect<'a, ResumeParticipantResult> {
        assert_eq!(request.child_seq_cutoff, Some(8));
        self.event("resume");
        Box::pin(async move {
            Ok(if self.newer_hold {
                ResumeParticipantResult::HeldByUser { child_seq: 9 }
            } else {
                ResumeParticipantResult::Resumed {
                    turn_id: Some("turn-2".into()),
                }
            })
        })
    }
    fn install<'a>(&'a self, _: &'a str, _: u64, _: ResumeCredential) -> Effect<'a, ()> {
        self.event("install");
        Box::pin(async { Ok(()) })
    }
    fn acknowledge_installed<'a>(&'a self, _: &'a str, _: u64, _: &'a str) -> Effect<'a, ()> {
        self.event("ack");
        Box::pin(async { Ok(()) })
    }
    fn cancel<'a>(&'a self, p: &'a PauseParticipant, parent: &'a str) -> Effect<'a, ()> {
        assert!(
            self.registry
                .require(parent)
                .expect("ledger")
                .participants
                .contains_key(&p.key)
        );
        self.event("cancel");
        Box::pin(async { Ok(()) })
    }
}
fn harness(dir: &std::path::Path, newer_hold: bool) -> (Arc<RunPauseController>, Arc<Effects>) {
    let registry = PauseRegistry::new(dir, AdmissionBarrier::new());
    let effects = Arc::new(Effects {
        registry: registry.clone(),
        events: Mutex::new(vec![]),
        newer_hold,
        guidance_entered: Mutex::new(None),
        guidance_release: Mutex::new(None),
    });
    (
        Arc::new(RunPauseController::new(registry, effects.clone())),
        effects,
    )
}
#[tokio::test]
async fn controller_orders_custody_guidance_install_release_and_cancel() {
    let dir = tempfile::tempdir().expect("dir");
    let (controller, effects) = harness(dir.path(), false);
    let stopping = controller.pause("root", 1);
    assert_eq!(
        controller.registry.barrier.state("root"),
        AdmissionState::Closed { pause_seq: 1 }
    );
    stopping.await.expect("stop");
    assert_eq!(
        controller
            .registry
            .require("root")
            .expect("ledger")
            .participants["a"]
            .ack_seq,
        Some(1)
    );
    effects.events.lock().expect("events").clear();
    assert_eq!(
        controller
            .resume("root", 1, "resume")
            .await
            .expect("resume"),
        RunResumeResult::Installed { failures: vec![] }
    );
    assert_eq!(
        *effects.events.lock().expect("events"),
        ["guidance", "resume", "install", "ack"]
    );
    assert!(matches!(
        controller.registry.barrier.enter("root", admission(), None),
        AdmissionEntry::Held { .. }
    ));
    assert!(controller.released("root", 1, "resume").expect("release"));
    assert_eq!(
        controller.registry.barrier.state("root"),
        AdmissionState::Open
    );
    controller
        .cancel("root", &RunRef::task("a"))
        .await
        .expect("cancel");
    assert!(
        controller
            .registry
            .require("root")
            .expect("ledger")
            .participants
            .is_empty()
    );
}
#[tokio::test]
async fn controller_preserves_newer_child_hold_and_frozen_cutoff() {
    let dir = tempfile::tempdir().expect("dir");
    let (controller, _) = harness(dir.path(), true);
    controller.pause("root", 1).await.expect("stop");
    controller
        .resume("root", 1, "resume")
        .await
        .expect("resume");
    let record = controller.registry.require("root").expect("ledger");
    assert_eq!(record.participants["a"].state, ParticipantState::HeldByUser);
    assert_eq!(record.participants["a"].observed_child_seq, Some(9));
    assert_eq!(record.resume.expect("intent").child_seq_cutoffs["a"], 8);
}
#[tokio::test]
async fn controller_stop_two_supersedes_resume_one_during_guidance() {
    let dir = tempfile::tempdir().expect("dir");
    let (controller, effects) = harness(dir.path(), false);
    controller.pause("root", 1).await.expect("stop");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    *effects.guidance_entered.lock().expect("entered") = Some(entered_tx);
    *effects.guidance_release.lock().expect("release") = Some(release_rx);
    let first_controller = controller.clone();
    let first = tokio::spawn(async move { first_controller.resume("root", 1, "resume-1").await });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
        .await
        .expect("bounded signal")
        .expect("entered");
    controller.pause("root", 2).await.expect("stop 2");
    release_tx.send(()).expect("release");
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .expect("bounded completion")
            .expect("join")
            .expect("resume"),
        RunResumeResult::Superseded
    );
    assert!(
        !controller
            .released("root", 1, "resume-1")
            .expect("stale release")
    );
    assert!(!effects.events.lock().expect("events").contains(&"resume"));
}

struct NativeEngine {
    result: Mutex<Option<super::native_child_pause::NativeResumeResult>>,
}
impl super::native_child_pause::NativeRunPause for NativeEngine {
    fn pause_for_run(
        &self,
        owner: RunPauseOwner,
    ) -> Result<EngineInheritReceipt, super::native_child_pause::NativePauseError> {
        Ok(if owner.parent_session_id == "root" {
            EngineInheritReceipt::Inherited { child_seq: 8 }
        } else {
            EngineInheritReceipt::WrongParent
        })
    }
    fn resume_for_run(
        &self,
        request: ResumeTurnRequest,
    ) -> Result<
        super::native_child_pause::NativeResumeResult,
        super::native_child_pause::NativePauseError,
    > {
        assert_eq!(request.owner.parent_session_id, "root");
        assert_eq!(request.child_seq_cutoff, 8);
        assert_eq!(request.resume_id, "resume");
        Ok(self
            .result
            .lock()
            .expect("result")
            .take()
            .expect("native response"))
    }
    fn hold_input(
        &self,
        parent: &str,
        _: HoldInput,
    ) -> Result<EngineHoldInputResult, super::native_child_pause::NativePauseError> {
        if parent != "root" {
            return Err(super::native_child_pause::NativePauseError::Engine(
                "wrong_parent".into(),
            ));
        }
        Ok(EngineHoldInputResult::Held {
            entry_id: "entry".into(),
            duplicate: false,
        })
    }
    fn observe_hold(
        &self,
        parent: &str,
    ) -> Result<Option<ObservedHold>, super::native_child_pause::NativePauseError> {
        Ok(Some(ObservedHold {
            held: parent == "root",
            child_seq: 8,
        }))
    }
}
struct NativeHandle {
    engine: Option<NativeEngine>,
}
impl crate::manager::ManagedChildHandle for NativeHandle {
    fn task_id(&self) -> &str {
        "child"
    }
    fn run_pause(&self) -> Option<&dyn super::native_child_pause::NativeRunPause> {
        self.engine
            .as_ref()
            .map(|e| e as &dyn super::native_child_pause::NativeRunPause)
    }
    fn session_id(&self) -> Option<String> {
        None
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, _: &str) -> Result<(), crate::host::HostError> {
        Ok(())
    }
    fn follow_up(&self, _: &str) -> Result<(), crate::host::HostError> {
        Ok(())
    }
    fn abort(&self) -> Result<(), crate::host::HostError> {
        Ok(())
    }
    fn subscribe(
        &self,
        _: crate::manager::child_handle::ManagedChildListener,
    ) -> crate::manager::child_handle::Unsubscribe {
        Box::new(|| {})
    }
    fn wait_for_outcome(&self) -> crate::runners::RunnerOutcome {
        crate::runners::RunnerOutcome::completed("old turn")
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), crate::host::HostError> {
        Ok(())
    }
}
fn owner() -> RunPauseOwner {
    RunPauseOwner {
        parent_session_id: "root".into(),
        parent_pause_seq: 1,
        run_ref: RunRef::task("a"),
    }
}
fn request() -> ResumeTurnRequest {
    ResumeTurnRequest {
        owner: owner(),
        resume_id: "resume".into(),
        child_seq_cutoff: 8,
    }
}
#[test]
fn native_adapter_refuses_missing_engines_and_forwards_exact_turn_receipt() {
    use super::native_child_pause::*;
    let port = NativeChildPausePort;
    let unavailable = NativeHandle { engine: None };
    assert!(matches!(
        port.pause_for_run(&unavailable, owner()),
        Err(NativePauseError::Unavailable { .. })
    ));
    assert_eq!(
        port.observe_hold(&unavailable, "root")
            .expect("optional view"),
        None
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    let child = NativeHandle {
        engine: Some(NativeEngine {
            result: Mutex::new(Some(NativeResumeResult::Turn(ResumedChildTurn {
                turn_id: "new-turn".into(),
                outcome: Box::new(move || {
                    receiver
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .expect("bounded exact turn outcome")
                }),
            }))),
        }),
    };
    assert_eq!(
        port.pause_for_run(&child, owner()).expect("pause"),
        EngineInheritReceipt::Inherited { child_seq: 8 }
    );
    let mut wrong_owner = owner();
    wrong_owner.parent_session_id = "other".into();
    assert_eq!(
        port.pause_for_run(&child, wrong_owner)
            .expect("wrong parent"),
        EngineInheritReceipt::WrongParent
    );
    let input = HoldInput {
        input_id: "g".into(),
        revision: 0,
        text: "guidance".into(),
    };
    assert!(port.hold_input(&child, input.clone(), "other").is_err());
    assert!(matches!(
        port.hold_input(&child, input, "root"),
        Ok(EngineHoldInputResult::Held { .. })
    ));
    assert_eq!(
        port.observe_hold(&child, "root").expect("view"),
        Some(ObservedHold {
            held: true,
            child_seq: 8
        })
    );
    let ResumeChildTurnResult::Resumed(turn) =
        port.resume_turn(&child, request()).expect("native resume")
    else {
        panic!("turn")
    };
    assert_eq!(turn.turn_id, "new-turn");
    sender
        .send(crate::runners::RunnerOutcome::completed("new result"))
        .expect("new result");
    assert_eq!(
        (turn.outcome)(),
        crate::runners::RunnerOutcome::completed("new result")
    );
    *child
        .engine
        .as_ref()
        .expect("engine")
        .result
        .lock()
        .expect("result") = Some(NativeResumeResult::OtherHoldLive);
    assert!(matches!(
        port.resume_turn(&child, request()).expect("held"),
        ResumeChildTurnResult::OtherHoldLive { .. }
    ));
    *child
        .engine
        .as_ref()
        .expect("engine")
        .result
        .lock()
        .expect("result") = Some(NativeResumeResult::Refused(ResumeRefusal::Stale));
    assert!(matches!(
        port.resume_turn(&child, request()),
        Err(NativePauseError::Refused {
            reason: ResumeRefusal::Stale,
            ..
        })
    ));
}
