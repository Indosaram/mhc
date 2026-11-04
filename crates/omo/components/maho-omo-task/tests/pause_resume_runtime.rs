//! A Resume that waits on a child turn the host runtime must drive cannot block that runtime.
//!
//! A native child admits its continue prompt on the host's current_thread runtime. The custody
//! handler for `held_wakes_released` runs on that same runtime, so the blocking Resume must run off
//! it; otherwise the prompt never runs and the session hangs forever.

use maho_ext_api::{EventKind, ExtensionEvent};
use maho_omo_task::engine_run_pause::{EngineRunPause, EngineRunPauseEffects, RunPauseSource};
use senpi_task::pause::{
    ports::RunRef,
    registry::{ParticipantKind, ParticipantState, PauseParticipant},
    run_controller::ResumeParticipantResult,
};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

struct ChildSource;
impl RunPauseSource for ChildSource {
    fn refs(&self, _: &str) -> Vec<RunRef> {
        Vec::new()
    }
    fn snapshot(&self, _: &str) -> Vec<PauseParticipant> {
        vec![PauseParticipant {
            key: "child".into(),
            kind: ParticipantKind::Task,
            run_ref: RunRef::task("child"),
            task_id: Some("child".into()),
            node_id: None,
            run_epoch: None,
            admission: None,
            state: ParticipantState::Pending,
            ack_seq: None,
            observed_child_seq: None,
            resume_turn_id: None,
            lost_reason: None,
            at: String::new(),
        }]
    }
}

#[test]
fn held_wakes_resume_does_not_starve_the_current_thread_host_runtime() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let host = runtime.handle().clone();
    let (resumed, resumed_seen) = mpsc::sync_channel(1);
    let effects = EngineRunPauseEffects {
        sources: Arc::new(Mutex::new(vec![Arc::new(ChildSource) as Arc<dyn RunPauseSource>])),
        task_records: Arc::new(Vec::new),
        pause_task: Arc::new(|_, _| {
            Ok(Some(senpi_task::manager::run_pause::EngineInheritReceipt { child_seq: 1 }))
        }),
        // Like a native child: admit the continue prompt on the host runtime, then wait for it.
        resume_task: Arc::new(move |_, _, _, _, _| {
            let (admitted, admission) = mpsc::sync_channel(1);
            host.spawn(async move {
                let _ = admitted.send(());
            });
            admission.recv().map_err(|error| error.to_string())?;
            let _ = resumed.try_send(());
            Ok(ResumeParticipantResult::Resumed { turn_id: None })
        }),
        observe_task_hold: Arc::new(|_, _| None),
        hold_task_input: Arc::new(|_, _| Err("unused".into())),
        fence_run: Arc::new(|_| Ok(())),
        release_fences: Arc::new(|_| Ok(())),
        cancel_task: Arc::new(|_| Ok(())),
    };
    let engine = Arc::new(EngineRunPause::new(dir.path(), Arc::new(effects)));
    let custody = maho_omo_task::event_bridge::TaskPauseCustody {
        bridge: Arc::new(maho_omo_task::run_pause_bridge::EngineRunPauseBridge::new(
            engine.clone(),
        )),
        session_id: Arc::new(|| Some("parent".into())),
    };
    let mut api = crate::support::api();
    let _subscription = custody.register(&mut api);
    api.events.emit(
        "session:hold",
        &serde_json::json!({"type":"session_paused", "kind":"session_hold", "pauseSeq":1}),
    );
    let handler = api.registered.handlers[&EventKind::HeldWakesReleased][0].clone();

    let (done, finished) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let context = crate::support::context();
        let mut event = ExtensionEvent::HeldWakesReleased {
            sources: Default::default(),
        };
        let result = runtime.block_on(handler(&mut event, &context));
        let _ = done.send(result.is_ok());
    });

    assert_eq!(
        finished.recv_timeout(Duration::from_secs(20)),
        Ok(true),
        "the Resume handler deadlocked the host runtime"
    );
    resumed_seen
        .recv_timeout(Duration::from_secs(1))
        .expect("the held child was resumed");
    assert_eq!(
        engine.admission_state("parent"),
        senpi_task::pause::admission_barrier::AdmissionState::Open
    );
}
