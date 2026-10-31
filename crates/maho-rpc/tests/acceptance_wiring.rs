//! senpi 4272-inprocess-acceptance / 4272-transcript-root-versions: the in-process acceptance
//! wiring. The registry bootstraps a lane before admission (never in a worker isolate), the writer
//! refuses a lane for a sealed session, and the binding applies the manager's transcript before
//! the record it arrives with.

use maho_core::{
    model_runtime::{CreateModelRuntimeOptions, ModelRuntime},
    sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session},
    session_manager::SessionManager,
    settings_manager::{InMemorySettingsStorage, SettingsManager},
};
use maho_rpc::{
    session_acceptance_lane::FrozenAcceptedRoot,
    session_binding::BindingAcceptance,
    session_event_fanout::SessionEventFanout,
    session_event_writer::SessionWriterActor,
    session_registry::{
        OpenAdmission, RpcSessionLaunchProfile, RpcSessionRegistry, SharedAcceptanceLane,
    },
};
use serde_json::json;
use std::sync::{Arc, Mutex};

async fn runtime_in(
    temp: &tempfile::TempDir,
) -> Arc<maho_core::agent_session_runtime::AgentSessionRuntime> {
    let cwd = temp.path().to_string_lossy().into_owned();
    let provider =
        maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
    let model = provider
        .get_model(Some("faux-1"))
        .expect("the faux provider registers faux-1");
    let models = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")),
        auth_path: Some(temp.path().join("auth.json")),
        providers: Some(vec![provider.provider]),
        ..Default::default()
    })
    .expect("valid model runtime");
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(cwd.clone()),
        model_runtime: Some(models),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(
            Box::new(InMemorySettingsStorage::default()),
            false,
        )),
        no_tools: Some(NoToolsMode::All),
        auto_title_sessions: Some(false),
        ..Default::default()
    })
    .await
    .expect("the session is created")
    .session;
    let registry = session.model_registry().clone();
    let services = maho_core::agent_session_services::AgentSessionServices {
        cwd: cwd.clone(),
        agent_dir: session.agent_dir(),
        auth_storage: Arc::clone(&registry.auth_storage),
        model_registry: registry,
        settings_manager: SettingsManager::from_storage(
            Box::new(InMemorySettingsStorage::default()),
            false,
        ),
        diagnostics: vec![],
    };
    Arc::new(maho_core::agent_session_runtime::AgentSessionRuntime::new(
        session,
        services,
        vec![],
        None,
        None,
    ))
}

/// Opens one session through admission and `install_runtime`; returns its lane, if any.
async fn open(
    temp: &tempfile::TempDir,
    worker_isolate: bool,
    compact_updates: bool,
) -> (
    Option<SharedAcceptanceLane>,
    Arc<maho_core::agent_session_runtime::AgentSessionRuntime>,
) {
    let runtime = runtime_in(temp).await;
    let mut registry = RpcSessionRegistry::default();
    registry.worker_isolate = worker_isolate;
    registry.compact_updates = Some(compact_updates);
    let OpenAdmission::Create(handle) = registry
        .admit_open(
            RpcSessionLaunchProfile {
                runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                    cwd: temp.path().to_string_lossy().into_owned(),
                    ..Default::default()
                },
                ..Default::default()
            },
            false,
            0.0,
        )
        .expect("admitted")
    else {
        panic!("expected create admission")
    };
    registry
        .install_runtime(&handle, Arc::clone(&runtime))
        .expect("installed");
    let lane = registry
        .get(&handle)
        .and_then(|entry| entry.acceptance.clone());
    (lane, runtime)
}

fn cut(lane: &SharedAcceptanceLane) -> FrozenAcceptedRoot {
    let mut lane = lane.lock().expect("lane lock");
    let root = lane.freeze();
    lane.release(&root);
    root
}

async fn finish(lane: SharedAcceptanceLane) {
    let mut lane = std::mem::take(&mut *lane.lock().expect("lane lock"));
    lane.retire(maho_rpc::session_acceptance_lane::RetireReason::Close);
    lane.idle().await;
}

#[tokio::test]
async fn the_registry_bootstraps_a_lane_before_admission_only_when_the_flag_is_on() {
    let temp = tempfile::tempdir().unwrap();
    let (lane, _runtime) = open(&temp, false, true).await;
    let lane = lane.expect("flag on: the in-process session owns a lane");
    let root = cut(&lane);
    assert_eq!(root.sequence, 0, "the baseline exists before any record");
    assert!(!root.session_epoch.is_empty());
    assert!(!lane.lock().unwrap().retired());
    finish(lane).await;

    let (lane, _runtime) = open(&temp, false, false).await;
    assert!(lane.is_none(), "flag off: no lane");
}

#[tokio::test]
async fn a_worker_isolate_registry_opens_no_lane_its_host_owns_it() {
    let temp = tempfile::tempdir().unwrap();
    let (lane, _runtime) = open(&temp, true, true).await;
    assert!(lane.is_none());
}

#[tokio::test]
async fn the_binding_applies_the_transcript_before_its_record_and_restarts_on_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let (lane, runtime) = open(&temp, false, true).await;
    let lane = lane.expect("flag on");
    let binding = BindingAcceptance::new(Arc::clone(&lane), runtime.session().clone());
    // Drains whatever the session journaled while opening, so `start` is the settled baseline.
    binding.accept(&json!({"type":"queue_update"}));
    let start = cut(&lane);

    binding.accept(&json!({"type":"queue_update"}));
    assert_eq!(
        cut(&lane).sequence,
        start.sequence,
        "pass-through takes no sequence"
    );

    runtime.session().with_session_manager_mut(|manager| {
        manager.append_message(json!({"role":"user","content":"hi","timestamp":1}))
    });
    let leaf = runtime
        .session()
        .with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
    assert_eq!(
        cut(&lane).transcript.history_version,
        start.transcript.history_version,
        "a committed append waits for the record it arrives with"
    );
    binding.accept(&json!({"type":"message_start","message":{"role":"user"}}));
    let accepted = cut(&lane);
    // At least the append's own transition, then the record: the transcript is sequenced first.
    assert!(accepted.sequence >= start.sequence + 2);
    assert!(accepted.transcript.history_version > start.transcript.history_version);
    assert_eq!(accepted.transcript.count, start.transcript.count + 1);
    assert_eq!(accepted.transcript.leaf_id, leaf);

    binding.accept(&json!({"type":"session_replaced"}));
    let replaced = cut(&lane);
    assert_ne!(
        replaced.session_epoch, start.session_epoch,
        "a new incarnation"
    );
    assert_eq!(replaced.sequence, 0);
    finish(lane).await;
}

#[tokio::test]
async fn the_writer_refuses_a_lane_for_a_sealed_session() {
    let (output, _reader) = tokio::io::duplex(64 * 1024);
    let writer = SessionWriterActor::new(output);
    let fanout = Mutex::new(SessionEventFanout::default());
    let lane: SharedAcceptanceLane = Arc::default();

    writer.register_acceptance(&fanout, "open", Arc::clone(&lane));
    let id = writer
        .reserve_close_response("sealed", &json!({"type":"response","id":"c"}), true)
        .expect("reservable")
        .expect("reserved");
    writer
        .complete_close_response(
            id,
            "sealed",
            json!({"type":"response","id":"c"}),
            true,
            None,
        )
        .expect("sealed");
    assert!(writer.is_session_sealed("sealed"));
    writer.register_acceptance(&fanout, "sealed", Arc::clone(&lane));

    let fanout = fanout.lock().unwrap();
    let lanes = fanout.acceptance.lock().unwrap();
    assert!(lanes.contains_key("open"));
    assert!(
        !lanes.contains_key("sealed"),
        "a sealed session takes no new lane"
    );
}

/// senpi session-registry-open.ts `ClientAdmissions.sealQueuedOnReplace` / `EffectLedger.sealOnReplace`: an installed
/// runtime replaced in place seals a held model switch in the ledger published under its host handle.
#[tokio::test]
async fn an_installed_session_replaced_in_place_seals_its_held_model_switch() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = runtime_in(&temp).await;
    let mut registry = RpcSessionRegistry::default();
    let OpenAdmission::Create(handle) = registry
        .admit_open(
            RpcSessionLaunchProfile {
                runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                    cwd: temp.path().to_string_lossy().into_owned(),
                    ..Default::default()
                },
                ..Default::default()
            },
            false,
            0.0,
        )
        .expect("admitted")
    else {
        panic!("expected create admission")
    };
    registry
        .install_runtime(&handle, Arc::clone(&runtime))
        .expect("installed");
    let ledger_session: Arc<dyn maho_rpc::effect_session::EffectSession> =
        Arc::new(RouteSession(handle.clone()));
    let ledger = maho_rpc::effect_ledger::EffectLedger::for_session(&ledger_session).await;
    ledger
        .begin(
            maho_rpc::effect_record::EffectCommandKind::SetModel,
            "m1",
            &json!({"provider":"p","modelId":"m"}),
            maho_rpc::effect_record::EffectActor {
                connection_id: "c".into(),
            },
        )
        .await
        .expect("begun");
    assert!(runtime.session().new_session(None).await.expect("new"));
    assert_eq!(
        ledger.status("m1", None).expect("recorded").state,
        maho_rpc::effect_record::EffectState::Failed
    );
    maho_rpc::effect_ledger::EffectLedger::forget(&handle);
    maho_rpc::client_admissions::ClientAdmissions::forget(&handle);
}

/// The ledger's view of a session keyed by a host handle; only what `begin` and `seal_on_replace` read.
struct RouteSession(String);
impl maho_rpc::effect_session::EffectSession for RouteSession {
    fn durable_session_id(&self) -> String {
        format!("durable-{}", self.0)
    }
    fn effect_route(&self) -> String {
        self.0.clone()
    }
    fn entries(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn append_custom_entry(&self, _: &str, _: serde_json::Value) {}
    fn persist_header_now(&self) -> futures::future::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn session_file(&self) -> Option<String> {
        None
    }
    fn is_streaming(&self) -> bool {
        false
    }
    fn current_abort_source(&self) -> Option<String> {
        None
    }
    fn streaming_client_message_id(&self) -> Option<String> {
        None
    }
    fn restore_queued_input(
        &self,
        _: maho_rpc::client_admission_record::PreparedAdmissionInput,
    ) -> futures::future::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn clear_queue(&self, _: bool) -> usize {
        0
    }
    fn queued_inputs(&self) -> Vec<maho_rpc::effect_session::QueuedInputView> {
        Vec::new()
    }
    fn change_queued_input(
        &self,
        _: &str,
        _: Option<String>,
        _: Option<u64>,
        _: Option<String>,
    ) -> futures::future::BoxFuture<'_, Result<maho_rpc::effect_session::QueuedChangeStatus, String>>
    {
        Box::pin(async { Ok(maho_rpc::effect_session::QueuedChangeStatus::NotFound) })
    }
    fn find_available_model(
        &self,
        _: &str,
        _: &str,
    ) -> futures::future::BoxFuture<'_, Result<Option<serde_json::Value>, String>> {
        Box::pin(async { Ok(None) })
    }
    fn set_model(
        &self,
        _: &str,
        _: &str,
        _: Option<String>,
    ) -> futures::future::BoxFuture<
        '_,
        Result<Option<String>, maho_rpc::effect_session::SetModelError>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn available_thinking_levels(&self) -> Vec<String> {
        Vec::new()
    }
    fn set_thinking_level(&self, _: &str, _: bool, _: Option<String>) {}
}
