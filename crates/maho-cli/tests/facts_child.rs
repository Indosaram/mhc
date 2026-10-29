//! End-to-end facts child session tests with the faux provider.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use maho_ai::providers::faux::{
    faux_assistant_message, faux_provider, faux_tool_call, FauxAssistantMessageOptions,
    FauxModelDefinition, FauxProviderHandle, RegisterFauxProviderOptions,
};
use maho_ai::types::StopReason;
use maho_cli::cli::facts_child::{CliFactsChildSpawner, FactsRuntimeFactory};
use maho_core::model_registry::ModelRegistry;
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use maho_core::sdk::HostRuntimeFactory;
use maho_omo_memory::facts_runner::FactsExtractorRunner;
use maho_omo_memory::facts_runner_types::FactsLaunchResult;
use maho_omo_memory::worker::facts_child_launch::FactsChildLaunchInput;
use maho_omo_memory::worker::memory_model_attempts::ReflectionModelCandidate;
use maho_omo_memory::worker::model_preflight::{Launcher, ModelPreflight};
use maho_omo_memory::worker::resolve_model::ReflectionModelResolution;
use maho_omo_memory::worker::spawn_supervisor::FactsChildOptions;
use memory_core::agreements::{AgreementAuthor, AgreementScope, FactsKnownAgreement};
use memory_core::facts::{
    person_routing::FactsPeopleRouting, queue::FactsAbortSignal, FactsPayload, FactsPrimaryHuman,
};
use tempfile::tempdir;

struct TestFixture {
    _root: tempfile::TempDir,
    runner: Arc<FactsExtractorRunner>,
    run_dir: PathBuf,
    batch_id: String,
    provider: FauxProviderHandle,
}

fn create_fixture(
    scheduler_hook: Option<Arc<dyn Fn() -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>>,
    extra_models: Vec<String>,
) -> TestFixture {
    let root = tempdir().expect("temp dir");
    let identity_paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
    let identity = memory_core::identity::resolve::MemoryIdentity {
        id: "agent".into(),
        safe_slug: "agent".into(),
        paths: identity_paths.clone(),
    };
    maho_omo_memory::engine_session::prepare_memory_engine_session(
        &identity.id,
        &identity.paths,
        Default::default(),
    )
    .expect("engine session");

    let mut models = vec![
        FauxModelDefinition {
            id: "faux-1".into(),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "faux-2".into(),
            ..Default::default()
        },
    ];
    for m in extra_models {
        models.push(FauxModelDefinition {
            id: m,
            ..Default::default()
        });
    }

    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        models: Some(models),
        scheduler_hook,
        ..Default::default()
    })
    .expect("valid faux provider");

    let mut runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        providers: Some(Vec::new()),
        ..Default::default()
    })
    .expect("model runtime");
    runtime.register_native_provider(provider.provider.clone());
    let registry = ModelRegistry::new(runtime.clone());
    registry
        .auth_storage
        .set(
            "faux",
            Some(serde_json::json!({"type": "api_key", "key": "test-key"})),
        )
        .expect("auth");

    let factory = HostRuntimeFactory {
        model_registry: registry,
        model_runtime: runtime,
        extension_factories: Vec::new(),
    };

    let spawner = CliFactsChildSpawner::with_factory(
        FactsRuntimeFactory::Ready(factory),
        root.path().join("agent").to_string_lossy().into_owned(),
    );

    let runner = Arc::new(FactsExtractorRunner::new_with_spawner(
        identity,
        Arc::new(|| 1700000000000),
        Some(spawner),
    ));

    let batch_id = "test-batch-001".to_string();
    let run_dir = maho_omo_memory::facts_run_storage::reserve_facts_run_dir(
        &maho_omo_memory::facts_run_storage::ReserveFactsRunDirOptions {
            facts_dir: &runner.identity.paths.facts,
            locks_dir: &runner.identity.paths.locks,
            entries: &[],
            batch_id: &batch_id,
            launched_at: 1700000000000,
            deadline_ms: Some(900_000),
            termination_grace_ms: Some(5_000),
            lock_wait_ms: None,
            receipts_dir: None,
            receipts: None,
            warn: None,
        },
    )
    .expect("reserve run dir")
    .expect("available run dir");

    TestFixture {
        _root: root,
        runner,
        run_dir,
        batch_id,
        provider,
    }
}

#[tokio::test]
async fn test_one_record_fact_call_writes_extraction_and_finalizes_cleanly() {
    let fixture = create_fixture(None, vec![]);
    let record_args = serde_json::json!({
        "scope": "project",
        "text": "The project uses Rust 2024",
        "date": "2026-10-25"
    });
    let tool_call = faux_tool_call(
        "record_fact",
        record_args.as_object().cloned().expect("object"),
        Some("call_1"),
    );
    let tool_msg = faux_assistant_message(
        tool_call,
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    );
    let done_msg = faux_assistant_message("done", Default::default());
    fixture
        .provider
        .set_responses(vec![tool_msg.into(), done_msg.into()]);

    let payload = FactsPayload {
        agreement_event_version: Some(1),
        version: 1,
        identity: "agent".into(),
        today: "2026-10-25".into(),
        known_people: vec![],
        primary_human: FactsPrimaryHuman {
            slug: "human".into(),
            aliases: vec![],
        },
        entries: vec![],
        agreements: None,
    };
    let run_id = fixture
        .run_dir
        .file_name()
        .expect("run dir name")
        .to_string_lossy()
        .into_owned();
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "faux/faux-1".into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    };
    let mut cache = ModelPreflight::default();
    let people = FactsPeopleRouting {
        enabled: true,
        max_entries: 40,
        max_entry_chars: 200,
    };
    let mut notes = Vec::new();
    let mut warn = |msg: &str| notes.push(msg.to_owned());
    let supervisor_args: Vec<String> = Vec::new();
    let input = FactsChildLaunchInput {
        sandbox: None,
        run_id: &run_id,
        run_dir: &fixture.run_dir,
        payload: (&payload).into(),
        oversized: false,
        agreements: false,
        record_limits: None,
        resolution: &resolution,
        env: &BTreeMap::new(),
        config_sources: &[],
        launch: &Launcher {
            command: "mock".into(),
            prefix_args: vec![],
        },
        hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
        options: FactsChildOptions {
            termination_grace_ms: 5000.0,
            max_output_bytes: 65536,
            supervisor_command: Path::new("mock"),
            supervisor_args: &supervisor_args,
            batch_id: &fixture.batch_id,
            queued: &[],
            launched_at: 1700000000000,
        },
        signal: None,
    };

    let result = fixture
        .runner
        .execute_child(&mut cache, input, &people, &mut warn)
        .await
        .expect("execute child");
    assert!(!matches!(result, FactsLaunchResult::Failed { .. }));
    let extraction = std::fs::read_to_string(fixture.run_dir.join("extraction.jsonl"))
        .expect("read extraction.jsonl");
    assert!(extraction.contains("The project uses Rust 2024"));
    assert_eq!(extraction.lines().count(), 1);
}

#[tokio::test]
async fn test_no_tool_call_yields_no_facts_not_invalid_extraction() {
    let fixture = create_fixture(None, vec![]);
    let msg = faux_assistant_message("No durable facts to record.", Default::default());
    fixture.provider.set_responses(vec![msg.into()]);

    let payload = FactsPayload {
        agreement_event_version: Some(1),
        version: 1,
        identity: "agent".into(),
        today: "2026-10-25".into(),
        known_people: vec![],
        primary_human: FactsPrimaryHuman {
            slug: "human".into(),
            aliases: vec![],
        },
        entries: vec![],
        agreements: None,
    };
    let run_id = fixture
        .run_dir
        .file_name()
        .expect("run dir name")
        .to_string_lossy()
        .into_owned();
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "faux/faux-1".into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    };
    let mut cache = ModelPreflight::default();
    let people = FactsPeopleRouting {
        enabled: true,
        max_entries: 40,
        max_entry_chars: 200,
    };
    let mut notes = Vec::new();
    let mut warn = |msg: &str| notes.push(msg.to_owned());
    let supervisor_args: Vec<String> = Vec::new();
    let input = FactsChildLaunchInput {
        sandbox: None,
        run_id: &run_id,
        run_dir: &fixture.run_dir,
        payload: (&payload).into(),
        oversized: false,
        agreements: false,
        record_limits: None,
        resolution: &resolution,
        env: &BTreeMap::new(),
        config_sources: &[],
        launch: &Launcher {
            command: "mock".into(),
            prefix_args: vec![],
        },
        hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
        options: FactsChildOptions {
            termination_grace_ms: 5000.0,
            max_output_bytes: 65536,
            supervisor_command: Path::new("mock"),
            supervisor_args: &supervisor_args,
            batch_id: &fixture.batch_id,
            queued: &[],
            launched_at: 1700000000000,
        },
        signal: None,
    };

    let result = fixture
        .runner
        .execute_child(&mut cache, input, &people, &mut warn)
        .await
        .expect("execute child");
    assert!(matches!(result, FactsLaunchResult::NoFacts { .. }));
    let extraction = std::fs::read_to_string(fixture.run_dir.join("extraction.jsonl"))
        .expect("read extraction.jsonl");
    assert!(extraction.is_empty());
}

#[tokio::test]
async fn test_abort_blocked_on_gate_ends_cancelled_with_no_records() {
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let gate_tx = Arc::new(std::sync::Mutex::new(Some(gate_tx)));
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);

    let scheduler_hook = Arc::new(move || {
        let gate_tx = gate_tx.clone();
        let mut release_rx = release_rx.clone();
        Box::pin(async move {
            if let Some(tx) = gate_tx.lock().expect("lock").take() {
                let _ = tx.send(());
            }
            let _ = release_rx.wait_for(|val| *val).await;
        }) as maho_ai::types::BoxFuture<'static, ()>
    });

    let fixture = create_fixture(Some(scheduler_hook), vec![]);

    let record_args = serde_json::json!({
        "scope": "project",
        "text": "Late fact after abort",
        "date": "2026-10-25"
    });
    let tool_call = faux_tool_call(
        "record_fact",
        record_args.as_object().cloned().expect("object"),
        Some("call_late"),
    );
    let tool_msg = faux_assistant_message(
        tool_call,
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    );
    fixture.provider.set_responses(vec![tool_msg.into()]);

    let payload = FactsPayload {
        agreement_event_version: Some(1),
        version: 1,
        identity: "agent".into(),
        today: "2026-10-25".into(),
        known_people: vec![],
        primary_human: FactsPrimaryHuman {
            slug: "human".into(),
            aliases: vec![],
        },
        entries: vec![],
        agreements: None,
    };
    let run_id = fixture
        .run_dir
        .file_name()
        .expect("run dir name")
        .to_string_lossy()
        .into_owned();
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "faux/faux-1".into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    };

    let signal = FactsAbortSignal::new();
    let child_signal = signal.clone();
    let runner = fixture.runner.clone();
    let run_dir = fixture.run_dir.clone();
    let batch_id = fixture.batch_id.clone();

    let task = tokio::spawn(async move {
        let mut cache = ModelPreflight::default();
        let people = FactsPeopleRouting {
            enabled: true,
            max_entries: 40,
            max_entry_chars: 200,
        };
        let mut notes = Vec::new();
        let mut warn = |msg: &str| notes.push(msg.to_owned());
        let supervisor_args: Vec<String> = Vec::new();
        let input = FactsChildLaunchInput {
            sandbox: None,
            run_id: &run_id,
            run_dir: &run_dir,
            payload: (&payload).into(),
            oversized: false,
            agreements: false,
            record_limits: None,
            resolution: &resolution,
            env: &BTreeMap::new(),
            config_sources: &[],
            launch: &Launcher {
                command: "mock".into(),
                prefix_args: vec![],
            },
            hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
            options: FactsChildOptions {
                termination_grace_ms: 5000.0,
                max_output_bytes: 65536,
                supervisor_command: Path::new("mock"),
                supervisor_args: &supervisor_args,
                batch_id: &batch_id,
                queued: &[],
                launched_at: 1700000000000,
            },
            signal: Some(child_signal),
        };
        runner
            .execute_child(&mut cache, input, &people, &mut warn)
            .await
    });

    // Await provider arriving at the gate with a bounded timeout (no sleeps)
    tokio::time::timeout(Duration::from_secs(5), gate_rx)
        .await
        .expect("gate reached within bounded timeout")
        .expect("gate channel open");

    // Abort the signal while provider is blocked on the gate
    signal.abort();

    // Release the gate so provider can exit
    let _ = release_tx.send(true);

    // Await child completion with bounded timeout (no sleeps)
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("child task finished within bounded timeout")
        .expect("task join successful")
        .expect("execute child completed");

    assert!(matches!(result, FactsLaunchResult::Failed { .. }));
    let extraction = std::fs::read_to_string(fixture.run_dir.join("extraction.jsonl"))
        .expect("read extraction.jsonl");
    assert!(
        extraction.is_empty(),
        "extraction.jsonl must have no records after abort"
    );
}

#[tokio::test]
async fn test_first_candidate_model_absent_falls_back_to_second() {
    let fixture = create_fixture(None, vec![]);
    // Model responds for second candidate
    fixture
        .provider
        .set_responses(vec![
            faux_assistant_message("Ran on fallback model.", Default::default()).into(),
        ]);

    let payload = FactsPayload {
        agreement_event_version: Some(1),
        version: 1,
        identity: "agent".into(),
        today: "2026-10-25".into(),
        known_people: vec![],
        primary_human: FactsPrimaryHuman {
            slug: "human".into(),
            aliases: vec![],
        },
        entries: vec![],
        agreements: None,
    };
    let run_id = fixture
        .run_dir
        .file_name()
        .expect("run dir name")
        .to_string_lossy()
        .into_owned();
    // First candidate model is not registered: "faux/absent-model-xyz"
    // Second candidate model is registered: "faux/faux-2"
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "faux/absent-model-xyz".into(),
        thinking: None,
        source: None,
        fallbacks: vec![ReflectionModelCandidate {
            model: "faux/faux-2".into(),
            thinking: None,
            load_extensions: None,
        }],
    };
    let mut cache = ModelPreflight::default();
    let people = FactsPeopleRouting {
        enabled: true,
        max_entries: 40,
        max_entry_chars: 200,
    };
    let mut notes = Vec::new();
    let mut warn = |msg: &str| notes.push(msg.to_owned());
    let supervisor_args: Vec<String> = Vec::new();
    let input = FactsChildLaunchInput {
        sandbox: None,
        run_id: &run_id,
        run_dir: &fixture.run_dir,
        payload: (&payload).into(),
        oversized: false,
        agreements: false,
        record_limits: None,
        resolution: &resolution,
        env: &BTreeMap::new(),
        config_sources: &[],
        launch: &Launcher {
            command: "mock".into(),
            prefix_args: vec![],
        },
        hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
        options: FactsChildOptions {
            termination_grace_ms: 5000.0,
            max_output_bytes: 65536,
            supervisor_command: Path::new("mock"),
            supervisor_args: &supervisor_args,
            batch_id: &fixture.batch_id,
            queued: &[],
            launched_at: 1700000000000,
        },
        signal: None,
    };

    let result = fixture
        .runner
        .execute_child(&mut cache, input, &people, &mut warn)
        .await
        .expect("execute child");
    assert!(matches!(result, FactsLaunchResult::NoFacts { .. }));
    assert_eq!(fixture.provider.get_call_log().len(), 1);
    assert_eq!(fixture.provider.get_call_log()[0].model_id, "faux-2");
}

#[tokio::test]
async fn test_agreements_payload_known_id_accepted_unknown_id_rejected() {
    let fixture = create_fixture(None, vec![]);
    // Turn 1: model attempts to reaffirm known agreement "0123456789" and unknown agreement "abcdef0123"
    let call1 = faux_tool_call(
        "record_agreement",
        serde_json::json!({
            "action": "reaffirm",
            "id": "0123456789",
            "date": "2026-10-25",
            "source": {
                "sessionId": "test-session",
                "messageId": "msg-1"
            }
        })
        .as_object()
        .cloned()
        .expect("object"),
        Some("call_agr_known"),
    );
    let call2 = faux_tool_call(
        "record_agreement",
        serde_json::json!({
            "action": "reaffirm",
            "id": "abcdef0123",
            "date": "2026-10-25",
            "source": {
                "sessionId": "test-session",
                "messageId": "msg-1"
            }
        })
        .as_object()
        .cloned()
        .expect("object"),
        Some("call_agr_unknown"),
    );
    let tool_msg = faux_assistant_message(
        vec![call1, call2],
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    );
    // Turn 2: model completes
    let done_msg = faux_assistant_message("Agreements processed.", Default::default());
    fixture
        .provider
        .set_responses(vec![tool_msg.into(), done_msg.into()]);

    let queue_entry = memory_core::facts::FactsQueueEntry {
        version: memory_core::facts::FACTS_QUEUE_VERSION,
        identity: "agent".into(),
        session_id: "test-session".into(),
        conversation_id: "test-conv".into(),
        range: memory_core::facts::FactsQueueRange {
            start_message_id: "msg-1".into(),
            end_message_id: "msg-1".into(),
            start_line: 0,
            end_snapshot_line: 1,
        },
        enqueued_at: "2026-10-25T12:00:00.000Z".into(),
        entries: vec![memory_core::journal::entries::TranscriptEntry::Text(
            memory_core::journal::entries::TextTranscriptEntry {
                kind: "user".into(),
                text: "Always use Bun".into(),
                captured_at: "2026-10-25T12:00:00.000Z".into(),
                source_line_id: "line-1".into(),
                source_message_id: "msg-1".into(),
            },
        )],
        lead: None,
    };

    let payload = FactsPayload {
        agreement_event_version: Some(1),
        version: 1,
        identity: "agent".into(),
        today: "2026-10-25".into(),
        known_people: vec![],
        primary_human: FactsPrimaryHuman {
            slug: "human".into(),
            aliases: vec![],
        },
        entries: vec![queue_entry],
        agreements: Some(vec![FactsKnownAgreement {
            id: "0123456789".into(),
            scope: AgreementScope::Global,
            by: AgreementAuthor::Human,
            text: "Always use Bun".into(),
            n: 1,
        }]),
    };
    let run_id = fixture
        .run_dir
        .file_name()
        .expect("run dir name")
        .to_string_lossy()
        .into_owned();
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "faux/faux-1".into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    };
    let mut cache = ModelPreflight::default();
    let people = FactsPeopleRouting {
        enabled: true,
        max_entries: 40,
        max_entry_chars: 200,
    };
    let mut notes = Vec::new();
    let mut warn = |msg: &str| notes.push(msg.to_owned());
    let supervisor_args: Vec<String> = Vec::new();
    let input = FactsChildLaunchInput {
        sandbox: None,
        run_id: &run_id,
        run_dir: &fixture.run_dir,
        payload: (&payload).into(),
        oversized: false,
        agreements: true,
        record_limits: None,
        resolution: &resolution,
        env: &BTreeMap::new(),
        config_sources: &[],
        launch: &Launcher {
            command: "mock".into(),
            prefix_args: vec![],
        },
        hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
        options: FactsChildOptions {
            termination_grace_ms: 5000.0,
            max_output_bytes: 65536,
            supervisor_command: Path::new("mock"),
            supervisor_args: &supervisor_args,
            batch_id: &fixture.batch_id,
            queued: &[],
            launched_at: 1700000000000,
        },
        signal: None,
    };

    let result = fixture
        .runner
        .execute_child(&mut cache, input, &people, &mut warn)
        .await
        .expect("execute child");
    assert!(!matches!(result, FactsLaunchResult::Failed { .. }));

    let agreements_path =
        fixture
            .run_dir
            .join(maho_omo_memory::facts_agreements::FACTS_AGREEMENTS_FILENAME);
    let agreements_content =
        std::fs::read_to_string(&agreements_path).expect("read agreements.jsonl");
    assert!(
        agreements_content.contains("0123456789"),
        "known agreement must be recorded"
    );
    assert!(
        !agreements_content.contains("abcdef0123"),
        "unknown agreement must not be recorded"
    );
    assert_eq!(
        agreements_content.lines().count(),
        1,
        "exactly one agreement line written"
    );
}

#[tokio::test]
async fn test_model_turn_error_stop_reason_fails_and_does_not_consume_queue() {
    let fixture = create_fixture(None, vec![]);

    // Configure faux provider to return an assistant message with stop_reason: Error
    let err_msg = faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("Request timed out.".into()),
            ..Default::default()
        },
    );
    fixture.provider.set_responses(vec![err_msg.into()]);

    // Create a mock queue entry and write it to the queue directory
    let queue_entry = memory_core::facts::FactsQueueEntry {
        lead: None,
        version: memory_core::facts::FACTS_QUEUE_VERSION,
        identity: "agent".into(),
        session_id: "test-session".into(),
        conversation_id: "test-conv".into(),
        range: memory_core::facts::FactsQueueRange {
            start_message_id: "msg-1".into(),
            end_message_id: "msg-2".into(),
            start_line: 0,
            end_snapshot_line: 1,
        },
        enqueued_at: "2026-10-25T12:00:00.000Z".into(),
        entries: vec![],
    };
    let queue_dir = fixture.runner.identity.paths.facts_queue.join("queue");
    std::fs::create_dir_all(&queue_dir).expect("create queue dir");
    let queue_file = queue_dir.join("20261025T120000000Z_test-conv_msg-2.json");
    std::fs::write(
        &queue_file,
        serde_json::to_string(&queue_entry).expect("serialize queue entry"),
    )
    .expect("write queue file");
    assert!(queue_file.exists(), "queue file must exist before execution");

    // Also test spawner directly to assert spawner result is completed: false with the error message
    let spawner = fixture.runner.spawner.as_ref().expect("spawner present");
    let cancel_state = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let direct_spawn_input = maho_omo_memory::facts_child_spawner::FactsChildSpawnInput {
        run_id: "test-direct-run".into(),
        run_dir: fixture.run_dir.clone(),
        model_candidates: vec![ReflectionModelCandidate {
            model: "faux/faux-1".into(),
            thinking: None,
            load_extensions: None,
        }],
        persona: memory_core::facts::load_facts_persona().to_owned(),
        prompt: "Extract durable facts".into(),
        custom_tools: vec![],
        tool_allowlist: vec![],
        hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
        signal: None,
        cancel_state: cancel_state.clone(),
    };
    let spawner_res = spawner
        .spawn(direct_spawn_input)
        .await
        .expect("spawn result");
    assert!(!spawner_res.completed, "spawner result must not be completed");
    assert_eq!(
        spawner_res.error_message.as_deref(),
        Some("Request timed out."),
        "spawner result must preserve error message"
    );

    // Re-set provider responses for the runner execution
    let err_msg2 = faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("Request timed out.".into()),
            ..Default::default()
        },
    );
    fixture.provider.set_responses(vec![err_msg2.into()]);

    let batch_id = "test-batch-err-002".to_string();
    let run_dir = maho_omo_memory::facts_run_storage::reserve_facts_run_dir(
        &maho_omo_memory::facts_run_storage::ReserveFactsRunDirOptions {
            facts_dir: &fixture.runner.identity.paths.facts,
            locks_dir: &fixture.runner.identity.paths.locks,
            entries: std::slice::from_ref(&queue_entry),
            batch_id: &batch_id,
            launched_at: 1700000000000,
            deadline_ms: Some(900_000),
            termination_grace_ms: Some(5_000),
            lock_wait_ms: None,
            receipts_dir: None,
            receipts: None,
            warn: None,
        },
    )
    .expect("reserve run dir")
    .expect("available run dir");

    let payload = FactsPayload {
        agreement_event_version: Some(1),
        version: 1,
        identity: "agent".into(),
        today: "2026-10-25".into(),
        known_people: vec![],
        primary_human: FactsPrimaryHuman {
            slug: "human".into(),
            aliases: vec![],
        },
        entries: vec![],
        agreements: None,
    };
    let run_id = run_dir
        .file_name()
        .expect("run dir name")
        .to_string_lossy()
        .into_owned();
    let resolution = ReflectionModelResolution::Resolved {
        category: "quick".into(),
        model: "faux/faux-1".into(),
        thinking: None,
        source: None,
        fallbacks: vec![],
    };
    let mut cache = ModelPreflight::default();
    let people = FactsPeopleRouting {
        enabled: true,
        max_entries: 40,
        max_entry_chars: 200,
    };
    let mut notes = Vec::new();
    let mut warn = |msg: &str| notes.push(msg.to_owned());
    let supervisor_args: Vec<String> = Vec::new();
    let queued_keys = vec![maho_omo_memory::facts_failure_recording::FactsQueuedKey {
        conversation_id: queue_entry.conversation_id.clone(),
        end_message_id: queue_entry.range.end_message_id.clone(),
        end_snapshot_line: queue_entry.range.end_snapshot_line,
    }];
    let input = FactsChildLaunchInput {
        sandbox: None,
        run_id: &run_id,
        run_dir: &run_dir,
        payload: (&payload).into(),
        oversized: false,
        agreements: false,
        record_limits: None,
        resolution: &resolution,
        env: &BTreeMap::new(),
        config_sources: &[],
        launch: &Launcher {
            command: "mock".into(),
            prefix_args: vec![],
        },
        hard_deadline_at: memory_core::support::time::now_millis() as f64 + 900_000.0,
        options: FactsChildOptions {
            termination_grace_ms: 5000.0,
            max_output_bytes: 65536,
            supervisor_command: Path::new("mock"),
            supervisor_args: &supervisor_args,
            batch_id: &batch_id,
            queued: &queued_keys,
            launched_at: 1700000000000,
        },
        signal: None,
    };

    let result = fixture
        .runner
        .execute_child(&mut cache, input, &people, &mut warn)
        .await
        .expect("execute child");

    // Must be Failed, NOT NoFacts or Committed
    assert!(
        matches!(result, FactsLaunchResult::Failed { .. }),
        "errored model turn must result in FactsLaunchResult::Failed"
    );
    assert!(
        !matches!(result, FactsLaunchResult::NoFacts { .. }),
        "errored model turn must not result in NoFacts"
    );

    // Queue file must still exist (not consumed)
    assert!(
        queue_file.exists(),
        "queue file must not be consumed when model turn errors"
    );

    // final.json must reflect the failure
    let final_path = run_dir.join("final.json");
    assert!(final_path.exists(), "final.json must exist");
    let final_val: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&final_path).expect("read final.json"))
            .expect("parse final.json");
    assert_eq!(final_val["outcome"], "failed");
    assert_eq!(final_val["detail"], "Request timed out.");
}
