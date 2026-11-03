//! Port of senpi `packages/agent/test/harness/runtime/drive-public.test.ts` (pin 903b8336).
//!
//! The pinned fixture is `AgentHarness.create({ session, models, model: faux.getModel(), ... })`
//! followed by `harness.lane("main")`; the Rust fixture builds the same shape through
//! `create_agent_harness_with_options`. Asynchronous ordering is driven by oneshot handshakes and
//! every wait is bounded, so no case depends on timing.

use maho_agent::harness::agent_harness::{
    AgentHarnessOptions, DriveOptions, DriveOptionsError, DriveOutcome,
};
use maho_agent::harness::context::{BACKGROUND_CONTEXT, with_cancel};
use maho_agent::harness::hooks::HookName;
use maho_agent::harness::runtime::harness::{Harness, create_agent_harness_with_options};
use maho_agent::harness::runtime::lane::{
    AbortError, CancelQueuedOutcome, Lane, NavigationOptions, OperationAdmission, PromptInput,
    QueuedInput, RunOutcome,
};
use maho_agent::harness::session::session::SessionErrorKind;
use maho_agent::harness::session::types::{
    LaneModelRef, NewEntry, OperationIntentKind, Session, SessionMetadata, SessionReader,
    TerminalStatus,
};
use maho_agent::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_agent::harness::types::{AgentHarnessResources, PromptTemplate, Skill};
use maho_ai::providers::faux::{
    FauxProviderHandle, FauxResponseStep, faux_assistant_message, faux_provider,
};
use maho_ai::types::{BoxFuture, DeferredOption, Message, UserContent, UserMessage};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(10);

struct PublicAuth;

impl maho_ai::models::ModelsAuth for PublicAuth {
    fn resolve<'a>(
        &'a self,
        _: &'a dyn maho_ai::models::Provider,
        _: &'a maho_ai::models::AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<maho_ai::models::AuthResolution>, maho_ai::models::ModelsError>>
    {
        Box::pin(async {
            Ok(Some(maho_ai::models::AuthResolution {
                ambient: false,
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some("fixture".into()),
                    ..Default::default()
                },
                env: None,
            }))
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        _: &'a dyn maho_ai::models::Provider,
        _: Option<&'a maho_ai::models::Credential>,
        _: &'a maho_ai::utils::abort::AbortSignal,
    ) -> BoxFuture<'a, Result<Option<maho_ai::models::Credential>, maho_ai::models::ModelsError>>
    {
        Box::pin(async { Ok(None) })
    }
}

struct Fixture {
    lane: Arc<Lane>,
    harness: Harness<()>,
    faux: FauxProviderHandle,
    session: Arc<StorageBackedSession>,
}

#[derive(Default)]
struct FixtureOptions {
    deferred: bool,
    resources: Option<AgentHarnessResources>,
}

async fn create_fixture(options: FixtureOptions) -> Fixture {
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "public-drive".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
            path: None,
        },
        Arc::new(MemoryStorage::new(MemoryStorageOptions::default())),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    let faux = faux_provider(Default::default()).expect("faux provider");
    let models = maho_ai::models::create_models(Some(maho_ai::models::CreateModelsOptions {
        auth: Some(Arc::new(PublicAuth)),
        ..Default::default()
    }));
    models.set_provider(faux.provider.clone());
    let (harness, _open) = create_agent_harness_with_options(
        AgentHarnessOptions::<()> {
            session: session.clone(),
            models,
            model: faux.get_model(None).expect("faux model"),
            thinking_level: None,
            active_tool_names: None,
            tools: Vec::new(),
            tool_context: None,
            system_prompt: None,
            resources: options.resources,
            stream_options: options.deferred.then(|| {
                maho_agent::harness::types::AgentHarnessStreamOptions {
                    deferred: Some(DeferredOption::Enabled(true)),
                    ..Default::default()
                }
            }),
            retry: None,
            compaction: None,
            steering_mode: None,
            follow_up_mode: None,
            tool_execution: None,
            to_provider_messages: None,
            entry_projectors: None,
        },
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("harness attaches");
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .expect("main lane");
    Fixture {
        lane,
        harness,
        faux,
        session,
    }
}

fn settings() -> maho_agent::harness::session::types::RunSettings {
    maho_agent::harness::session::types::RunSettings {
        compaction: maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
        steering_mode: maho_agent::types::QueueMode::All,
        follow_up_mode: maho_agent::types::QueueMode::All,
        tool_execution: maho_agent::harness::session::types::ToolExecutionMode::Parallel,
    }
}

fn text(value: &str) -> PromptInput {
    PromptInput::Text {
        text: value.into(),
        images: vec![],
    }
}

fn user(content: &str, timestamp: i64) -> maho_agent::types::AgentMessage {
    maho_agent::types::AgentMessage::from(Message::User(UserMessage {
        content: UserContent::Text(content.into()),
        timestamp,
    }))
}

async fn accept_run(lane: &Lane, operation_id: &str) -> OperationAdmission {
    lane.accept_prompt(
        text(operation_id),
        Some(operation_id.into()),
        settings(),
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("acceptance does not fault")
    .expect("acceptance admits")
}

fn drive_options(operation_id: &str) -> DriveOptions {
    DriveOptions {
        operation_id: operation_id.into(),
        wait_for_retry: None,
        poll_deferred: None,
    }
}

async fn drive(lane: &Arc<Lane>, operation_id: &str) -> Result<DriveOutcome, DriveOptionsError> {
    tokio::time::timeout(
        BOUND,
        lane.drive(drive_options(operation_id), &BACKGROUND_CONTEXT),
    )
    .await
    .expect("drive deadline")
}

fn settled(outcome: &DriveOutcome) -> &maho_agent::harness::session::types::OperationResultRecord {
    let DriveOutcome::Settled { outcome } = outcome else {
        panic!("expected a settled drive, got {outcome:?}")
    };
    outcome
}

fn settled_run(result: RunOutcome) -> maho_agent::harness::session::types::OperationResultRecord {
    let RunOutcome::Settled(record) = result else {
        panic!("expected a settled run, got {result:?}")
    };
    record
}

/// A faux response step that signals `started`, then waits for `release` before answering.
fn gated_answer(
    answer: &str,
) -> (
    FauxResponseStep,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let answer = answer.to_owned();
    let step = FauxResponseStep::Factory(Arc::new(
        move |_: &maho_ai::types::Context,
              _: Option<&maho_ai::types::SimpleStreamOptions>,
              _: &maho_ai::providers::faux::FauxProviderState,
              _: &maho_ai::types::Model| {
            let started = started_tx.lock().expect("started").take();
            let release = release_rx.clone();
            let answer = answer.clone();
            Box::pin(async move {
                if let Some(started) = started {
                    let _ = started.send(());
                }
                if let Some(release) = release.lock().await.take() {
                    let _ = release.await;
                }
                faux_assistant_message(answer.as_str(), Default::default())
            })
        },
    ));
    (step, started_rx, release_tx)
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future).await.expect("deadline")
}

#[tokio::test]
async fn persists_model_identity_without_requiring_a_local_registration() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    fixture
        .lane
        .set_model(
            LaneModelRef {
                provider: "missing".into(),
                model_id: "missing-model".into(),
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("set model");

    assert!(fixture.lane.get_model().expect("get model").is_none());
    let record = settled_run(
        bounded(fixture.lane.prompt(text("prompt"), &BACKGROUND_CONTEXT))
            .await
            .expect("prompt does not fault")
            .expect("prompt admits"),
    );
    assert_eq!(record.kind, OperationIntentKind::Run);
    assert_eq!(record.status, TerminalStatus::Failed);
    assert_eq!(
        record.error.as_ref().map(|error| error.code.as_str()),
        Some("model_unavailable")
    );
    assert_eq!(fixture.faux.state().call_count, 0);
}

#[tokio::test]
async fn composes_prompt_skill_and_template_acceptance_with_drive() {
    let fixture = create_fixture(FixtureOptions {
        resources: Some(AgentHarnessResources {
            skills: Some(vec![Skill {
                name: "review".into(),
                description: "Review".into(),
                content: "Inspect it".into(),
                file_path: "/skills/review/SKILL.md".into(),
                disable_model_invocation: None,
            }]),
            prompt_templates: Some(vec![PromptTemplate {
                name: "fix".into(),
                description: None,
                content: "Fix $1".into(),
            }]),
        }),
        ..Default::default()
    })
    .await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("prompt answer", Default::default()).into(),
        faux_assistant_message("skill answer", Default::default()).into(),
        faux_assistant_message("template answer", Default::default()).into(),
    ]);

    for result in [
        bounded(fixture.lane.prompt(text("prompt"), &BACKGROUND_CONTEXT)).await,
        bounded(
            fixture
                .lane
                .skill("review", Some("strict".into()), &BACKGROUND_CONTEXT),
        )
        .await,
        bounded(
            fixture
                .lane
                .prompt_from_template("fix", vec!["it".into()], &BACKGROUND_CONTEXT),
        )
        .await,
    ] {
        let record = settled_run(result.expect("no fault").expect("admitted"));
        assert_eq!(record.kind, OperationIntentKind::Run);
        assert_eq!(record.status, TerminalStatus::Completed);
    }
    assert_eq!(fixture.faux.state().call_count, 3);
}

#[tokio::test]
async fn returns_a_convenience_only_suspension_observation() {
    let fixture = create_fixture(FixtureOptions {
        deferred: true,
        ..Default::default()
    })
    .await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("eventual answer", Default::default()).into(),
    ]);

    let outcome = bounded(fixture.lane.prompt(text("defer"), &BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect("admitted");
    let RunOutcome::Suspended(suspended) = outcome else {
        panic!("expected a suspended run, got {outcome:?}")
    };
    assert!(!suspended.operation_id.is_empty());
    let model = fixture.faux.get_model(None).expect("faux model");
    assert_eq!(suspended.deferred.provider, model.provider);
    assert_eq!(suspended.deferred.model_id, model.id);
    assert!(matches!(
        fixture
            .lane
            .state()
            .operation
            .map(|operation| operation.state),
        Some(maho_agent::harness::session::types::OperationState::DeferredSuspended(_))
    ));
}

#[tokio::test]
async fn records_caller_usage_as_an_adjustment_and_publishes_committed_totals() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let usage = faux_assistant_message("usage", Default::default()).usage;
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = fixture.harness.events.on(
        "usage",
        Arc::new(move |event, _| {
            sink.lock()
                .expect("events")
                .push(maho_agent::harness::events::event_to_value(&event));
            Box::pin(async {})
        }),
    );

    let usage_id = fixture
        .lane
        .record_usage(
            usage,
            Some("external".into()),
            Some(serde_json::json!({"source": "test"})),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("record usage");

    let totals = fixture
        .session
        .get_stats(&BACKGROUND_CONTEXT)
        .await
        .expect("stats")
        .usage;
    let events = events.lock().expect("events").clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "usage");
    assert_eq!(events[0]["lane"], "main");
    assert_eq!(events[0]["row"]["id"], serde_json::json!(usage_id));
    assert_eq!(events[0]["row"]["adjustment"], true);
    assert_eq!(
        events[0]["totals"],
        serde_json::to_value(totals).expect("totals json")
    );
}

#[tokio::test]
async fn starts_an_ordinary_continuation_run_from_queued_input() {
    for kind in ["steer", "followUp", "nextRun"] {
        let fixture = create_fixture(FixtureOptions::default()).await;
        fixture
            .lane
            .append_message(user("history", 1), &BACKGROUND_CONTEXT)
            .await
            .expect("history");
        let (summary, started, release) = gated_answer("summary");
        fixture.faux.set_responses(vec![
            summary,
            faux_assistant_message("continuation answer", Default::default()).into(),
        ]);
        let lane = fixture.lane.clone();
        let compacting = tokio::spawn(async move { lane.compact(None, &BACKGROUND_CONTEXT).await });
        bounded(started).await.expect("summary started");
        let input = QueuedInput::Text("continue".into());
        match kind {
            "steer" => fixture.lane.steer(input, vec![], &BACKGROUND_CONTEXT).await,
            "followUp" => {
                fixture
                    .lane
                    .follow_up(input, vec![], &BACKGROUND_CONTEXT)
                    .await
            }
            _ => {
                fixture
                    .lane
                    .next_run(input, vec![], &BACKGROUND_CONTEXT)
                    .await
            }
        }
        .expect("queued");
        release.send(()).expect("release summary");

        let outcome = bounded(compacting)
            .await
            .expect("compaction task")
            .expect("no fault")
            .expect("admitted");
        assert_eq!(
            outcome.compaction.kind,
            OperationIntentKind::Compaction,
            "{kind}"
        );
        assert_eq!(
            outcome.compaction.status,
            TerminalStatus::Completed,
            "{kind}"
        );
        let run = settled_run(outcome.run.expect("continuation run"));
        assert_eq!(run.kind, OperationIntentKind::Run, "{kind}");
        assert_eq!(run.status, TerminalStatus::Completed, "{kind}");
        assert_eq!(fixture.faux.state().call_count, 2, "{kind}");
    }
}

#[tokio::test]
async fn lets_a_competing_acceptance_win_the_structural_continuation_window() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    fixture
        .lane
        .append_message(user("history", 1), &BACKGROUND_CONTEXT)
        .await
        .expect("history");
    let (summary, started, release) = gated_answer("summary");
    fixture.faux.set_responses(vec![
        summary,
        faux_assistant_message("competitor answer", Default::default()).into(),
    ]);
    let competing = Arc::new(tokio::sync::Mutex::new(None));
    let slot = competing.clone();
    let competitor_lane = Arc::downgrade(&fixture.lane);
    let _subscription = fixture.harness.events.on(
        "compaction_end",
        Arc::new(move |event, _| {
            let value = maho_agent::harness::events::event_to_value(&event);
            let slot = slot.clone();
            let lane = competitor_lane.upgrade();
            Box::pin(async move {
                if value["status"] == "completed"
                    && let Some(lane) = lane
                {
                    let accepting = tokio::spawn(async move {
                        lane.accept_prompt(
                            text("competitor"),
                            None,
                            settings(),
                            &BACKGROUND_CONTEXT,
                        )
                        .await
                    });
                    *slot.lock().await = Some(accepting);
                }
            })
        }),
    );
    let lane = fixture.lane.clone();
    let compacting = tokio::spawn(async move { lane.compact(None, &BACKGROUND_CONTEXT).await });
    bounded(started).await.expect("summary started");
    fixture
        .lane
        .next_run(
            QueuedInput::Text("queued".into()),
            vec![],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("queued");
    release.send(()).expect("release summary");

    let compacted = bounded(compacting)
        .await
        .expect("compaction task")
        .expect("no fault")
        .expect("admitted");
    assert_eq!(compacted.compaction.status, TerminalStatus::Completed);
    assert!(compacted.run.is_none());
    let accepting = competing
        .lock()
        .await
        .take()
        .expect("competing acceptance did not start");
    let admission = bounded(accepting)
        .await
        .expect("acceptance task")
        .expect("no fault")
        .expect("competitor admitted");
    let outcome = drive(&fixture.lane, &admission.operation_id)
        .await
        .expect("drive");
    assert_eq!(settled(&outcome).status, TerminalStatus::Completed);
}

#[tokio::test]
async fn cancels_queued_input_and_reports_consumed_or_missing_ids() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let cancelled = fixture
        .lane
        .next_run(
            QueuedInput::Text("cancel".into()),
            vec![],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("queued");
    assert_eq!(
        fixture
            .lane
            .cancel_queued(cancelled.clone(), &BACKGROUND_CONTEXT)
            .await
            .expect("cancel"),
        CancelQueuedOutcome::Cancelled
    );
    assert_eq!(
        fixture
            .lane
            .cancel_queued(cancelled, &BACKGROUND_CONTEXT)
            .await
            .expect("cancel"),
        CancelQueuedOutcome::NotFound
    );

    let consumed = fixture
        .lane
        .next_run(
            QueuedInput::Text("consume".into()),
            vec![],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("queued");
    let admission = fixture
        .lane
        .accept_prompt(text(""), None, settings(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("admitted");
    assert_eq!(
        fixture
            .lane
            .cancel_queued(consumed, &BACKGROUND_CONTEXT)
            .await
            .expect("cancel"),
        CancelQueuedOutcome::AlreadyConsumed
    );
    fixture.faux.set_responses(vec![
        faux_assistant_message("answer", Default::default()).into(),
    ]);
    drive(&fixture.lane, &admission.operation_id)
        .await
        .expect("drive");
}

#[tokio::test]
async fn admits_input_after_cancellation_and_preserves_it_through_reconciliation() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "cancelled").await;
    fixture
        .lane
        .request_operation_abort("cancelled".into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort requested");
    fixture
        .lane
        .steer(
            QueuedInput::Text("late".into()),
            vec![],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("late steer");
    let outcome = drive(&fixture.lane, "cancelled").await.expect("drive");
    assert_eq!(settled(&outcome).status, TerminalStatus::Aborted);
    assert_eq!(fixture.lane.state().inbox.len(), 1);

    fixture.faux.set_responses(vec![
        faux_assistant_message("late answer", Default::default()).into(),
    ]);
    let record = settled_run(
        bounded(fixture.lane.prompt(text(""), &BACKGROUND_CONTEXT))
            .await
            .expect("no fault")
            .expect("admitted"),
    );
    assert_eq!(record.kind, OperationIntentKind::Run);
    assert_eq!(record.status, TerminalStatus::Completed);
}

#[tokio::test]
async fn composes_standalone_compaction_acceptance_with_drive() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    fixture
        .lane
        .append_message(user("history", 1), &BACKGROUND_CONTEXT)
        .await
        .expect("history");
    fixture.faux.set_responses(vec![
        faux_assistant_message("summary", Default::default()).into(),
    ]);

    let outcome = bounded(fixture.lane.compact(None, &BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect("admitted");
    assert_eq!(outcome.compaction.kind, OperationIntentKind::Compaction);
    assert_eq!(outcome.compaction.status, TerminalStatus::Completed);
    assert_eq!(fixture.faux.state().call_count, 1);
}

#[tokio::test]
async fn composes_summarized_and_unsummarized_navigation_acceptance_with_drive() {
    for summarize in [false, true] {
        let fixture = create_fixture(FixtureOptions::default()).await;
        let root_id = fixture
            .lane
            .append_message(user("root", 1), &BACKGROUND_CONTEXT)
            .await
            .expect("root");
        fixture
            .lane
            .append_message(user("source", 2), &BACKGROUND_CONTEXT)
            .await
            .expect("source");
        let mutation = fixture
            .session
            .begin_mutation(&BACKGROUND_CONTEXT)
            .await
            .expect("mutation");
        mutation
            .commit(
                vec![maho_agent::harness::session::commit::insert_entry(
                    NewEntry::message("target", Some(root_id), user("target", 3)),
                )],
                &BACKGROUND_CONTEXT,
            )
            .await
            .expect("target entry");
        mutation.end(&BACKGROUND_CONTEXT).await;
        if summarize {
            fixture.faux.set_responses(vec![
                faux_assistant_message("branch summary", Default::default()).into(),
            ]);
        }

        let outcome = bounded(fixture.lane.navigate_tree(
            Some("target".into()),
            NavigationOptions {
                summarize,
                label: Some("chosen".into()),
                custom_instructions: None,
            },
            &BACKGROUND_CONTEXT,
        ))
        .await
        .expect("no fault")
        .expect("admitted");
        assert_eq!(outcome.navigation.kind, OperationIntentKind::Navigation);
        assert_eq!(outcome.navigation.status, TerminalStatus::Completed);
        assert_eq!(
            fixture.faux.state().call_count,
            u64::from(summarize),
            "summarize={summarize}"
        );
        assert!(fixture.lane.get_tip_id().expect("tip").is_some());
    }
}

#[tokio::test]
async fn resumes_any_current_operation_after_acceptance_or_reopen() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "run").await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("answer", Default::default()).into(),
    ]);

    let outcome = bounded(fixture.lane.resume(&BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect("resumed");
    let record = settled(&outcome);
    assert_eq!(record.operation_id, "run");
    assert_eq!(record.kind, OperationIntentKind::Run);
    assert_eq!(record.status, TerminalStatus::Completed);
}

#[tokio::test]
async fn polls_one_deferred_permit_through_resume() {
    let fixture = create_fixture(FixtureOptions {
        deferred: true,
        ..Default::default()
    })
    .await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("eventual answer", Default::default()).into(),
    ]);
    let suspended = bounded(fixture.lane.prompt(text("defer"), &BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect("admitted");
    let RunOutcome::Suspended(suspended) = suspended else {
        panic!("Expected suspended run")
    };

    let outcome = bounded(fixture.lane.resume(&BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect("resumed");
    let record = settled(&outcome);
    assert_eq!(record.operation_id, suspended.operation_id);
    assert_eq!(record.kind, OperationIntentKind::Run);
    assert_eq!(record.status, TerminalStatus::Completed);
    assert!(fixture.lane.state().operation.is_none());
    assert_eq!(fixture.faux.state().deferred_fetch_count, 1);
    let message = bounded(fixture.lane.resume(&BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect_err("nothing to resume");
    assert!(
        message.contains("no active operation to resume"),
        "{message}"
    );
}

#[tokio::test]
async fn aborts_and_reconciles_the_current_operation() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "run").await;

    let aborted = bounded(fixture.lane.abort(&BACKGROUND_CONTEXT))
        .await
        .expect("no fault")
        .expect("aborted");
    assert_eq!(aborted.operation_id, "run");
    assert!(aborted.steer.is_empty());
    assert!(aborted.follow_up.is_empty());
    assert!(fixture.lane.state().operation.is_none());
    assert_eq!(fixture.faux.state().call_count, 0);
    assert!(matches!(
        bounded(fixture.lane.abort(&BACKGROUND_CONTEXT))
            .await
            .expect("no fault"),
        Err(AbortError::NoActiveOperation { .. })
    ));
}

#[tokio::test]
async fn waits_for_an_operation_that_has_no_installed_drive() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "run").await;
    // The operation is open and no drive is installed, so the waiter must not settle.
    let waiting = fixture.lane.wait_for_idle(&BACKGROUND_CONTEXT);
    tokio::pin!(waiting);
    assert!(futures::poll!(waiting.as_mut()).is_pending());

    fixture.faux.set_responses(vec![
        faux_assistant_message("answer", Default::default()).into(),
    ]);
    drive(&fixture.lane, "run").await.expect("drive");
    bounded(waiting).await.expect("idle");
}

#[tokio::test]
async fn serializes_concurrent_run_when_idle_callbacks() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel::<()>();
    let first_lane = fixture.lane.clone();
    let first_order = order.clone();
    let first = tokio::spawn(async move {
        first_lane
            .run_when_idle(
                move |_| async move {
                    first_order.lock().expect("order").push("first:start");
                    first_started_tx.send(()).expect("first started");
                    release_first_rx.await.expect("release first");
                    first_order.lock().expect("order").push("first:end");
                    Ok(())
                },
                &BACKGROUND_CONTEXT,
            )
            .await
    });
    bounded(first_started_rx).await.expect("first started");
    let second_order = order.clone();
    let second = fixture.lane.run_when_idle(
        move |_| async move {
            second_order.lock().expect("order").push("second");
            Ok(())
        },
        &BACKGROUND_CONTEXT,
    );
    tokio::pin!(second);
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert_eq!(*order.lock().expect("order"), vec!["first:start"]);

    release_first_tx.send(()).expect("release first");
    bounded(first).await.expect("first task").expect("first");
    bounded(second).await.expect("second");
    assert_eq!(
        *order.lock().expect("order"),
        vec!["first:start", "first:end", "second"]
    );
}

#[tokio::test]
async fn owns_the_idle_window_while_run_when_idle_executes() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let callback_lane = fixture.lane.clone();
    let callback = tokio::spawn(async move {
        callback_lane
            .run_when_idle(
                move |_| async move {
                    started_tx.send(()).expect("callback started");
                    release_rx.await.expect("release callback");
                    Ok(())
                },
                &BACKGROUND_CONTEXT,
            )
            .await
    });
    bounded(started_rx).await.expect("callback started");
    let acceptance =
        fixture
            .lane
            .accept_prompt(text("after"), None, settings(), &BACKGROUND_CONTEXT);
    tokio::pin!(acceptance);
    assert!(futures::poll!(acceptance.as_mut()).is_pending());

    release_tx.send(()).expect("release callback");
    bounded(callback)
        .await
        .expect("callback task")
        .expect("callback");
    let admission = bounded(acceptance)
        .await
        .expect("no fault")
        .expect("admitted");
    fixture.faux.set_responses(vec![
        faux_assistant_message("answer", Default::default()).into(),
    ]);
    drive(&fixture.lane, &admission.operation_id)
        .await
        .expect("drive");
}

#[tokio::test]
async fn allows_coherent_lane_reads_from_an_idle_callback() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let lane = fixture.lane.clone();
    let events = fixture.harness.events.clone();
    bounded(fixture.lane.run_when_idle(
        move |context| async move {
            let execution = lane.inspect_execution(&context).await?;
            assert!(execution.current.is_none());
            let watcher = maho_agent::harness::runtime::transcript::watch_lane(
                lane.clone() as Arc<dyn maho_agent::harness::runtime::types::RuntimeLane>,
                events,
                &context,
            )
            .await?;
            let snapshot = watcher.snapshot().expect("watch snapshot");
            assert!(snapshot["operation"].is_null());
            watcher.unsubscribe();
            Ok(())
        },
        &BACKGROUND_CONTEXT,
    ))
    .await
    .expect("idle callback");
}

#[tokio::test]
async fn releases_idle_ownership_when_the_callback_fails() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let failure = maho_agent::harness::session::session::session_invariant_error("callback failed");
    let expected = failure.clone();
    let error = bounded(
        fixture
            .lane
            .run_when_idle(move |_| async move { Err(failure) }, &BACKGROUND_CONTEXT),
    )
    .await
    .expect_err("callback failure propagates");
    assert_eq!(error, expected);
    assert!(
        bounded(
            fixture
                .lane
                .accept_prompt(text("after"), None, settings(), &BACKGROUND_CONTEXT)
        )
        .await
        .expect("no fault")
        .is_ok()
    );
}

#[tokio::test]
async fn close_waits_for_an_already_running_idle_callback() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let callback_lane = fixture.lane.clone();
    let callback = tokio::spawn(async move {
        callback_lane
            .run_when_idle(
                move |_| async move {
                    started_tx.send(()).expect("callback started");
                    release_rx.await.expect("release callback");
                    Ok(())
                },
                &BACKGROUND_CONTEXT,
            )
            .await
    });
    bounded(started_rx).await.expect("callback started");
    let closing = fixture.harness.close(&BACKGROUND_CONTEXT);
    tokio::pin!(closing);
    assert!(futures::poll!(closing.as_mut()).is_pending());

    release_tx.send(()).expect("release callback");
    bounded(callback)
        .await
        .expect("callback task")
        .expect("callback");
    bounded(closing).await;
}

#[tokio::test]
async fn installs_one_pass_and_joins_same_operation_callers() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let (answer, started, release) = gated_answer("answer");
    fixture.faux.set_responses(vec![answer]);
    let admission = accept_run(&fixture.lane, "run").await;

    let first_lane = fixture.lane.clone();
    let first_id = admission.operation_id.clone();
    let first = tokio::spawn(async move { drive(&first_lane, &first_id).await });
    bounded(started).await.expect("provider started");
    let second_lane = fixture.lane.clone();
    let second_id = admission.operation_id.clone();
    let second = tokio::spawn(async move { drive(&second_lane, &second_id).await });
    assert_eq!(
        fixture.lane.active_drive_operation_id(),
        Some(admission.operation_id.clone())
    );
    release.send(()).expect("release provider");

    let first = bounded(first)
        .await
        .expect("first task")
        .expect("first drive");
    let second = bounded(second)
        .await
        .expect("second task")
        .expect("second drive");
    assert_eq!(settled(&first).operation_id, "run");
    assert_eq!(settled(&first).status, TerminalStatus::Completed);
    assert_eq!(second, first);
    assert_eq!(fixture.faux.state().call_count, 1);
    bounded(fixture.lane.wait_for_idle(&BACKGROUND_CONTEXT))
        .await
        .expect("idle");
    assert!(fixture.lane.active_drive_operation_id().is_none());
}

#[tokio::test]
async fn returns_old_result_records_without_disturbing_the_current_operation() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("first", Default::default()).into(),
    ]);
    accept_run(&fixture.lane, "first").await;
    let first = drive(&fixture.lane, "first").await.expect("drive first");
    assert_eq!(settled(&first).operation_id, "first");
    accept_run(&fixture.lane, "second").await;

    let replay = drive(&fixture.lane, "first").await.expect("replay first");
    assert_eq!(settled(&replay).operation_id, "first");
    assert_eq!(
        fixture
            .lane
            .state()
            .operation
            .map(|operation| operation.meta.operation_id),
        Some("second".into())
    );
    assert!(fixture.lane.active_drive_operation_id().is_none());

    fixture.faux.set_responses(vec![
        faux_assistant_message("second", Default::default()).into(),
    ]);
    let second = drive(&fixture.lane, "second").await.expect("drive second");
    assert_eq!(settled(&second).operation_id, "second");
}

#[tokio::test]
async fn isolates_stale_operation_ids() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "current").await;

    let Err(DriveOptionsError::Mismatch(mismatch)) = drive(&fixture.lane, "stale").await else {
        panic!("stale drive must report a mismatch")
    };
    assert_eq!(mismatch.expected, "stale");
    assert_eq!(mismatch.current_operation_id.as_deref(), Some("current"));
    assert!(fixture.lane.active_drive_operation_id().is_none());
    assert_eq!(fixture.faux.state().call_count, 0);
}

#[tokio::test]
async fn does_not_install_for_a_caller_already_cancelled() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "run").await;
    let (context, handle) = with_cancel(&BACKGROUND_CONTEXT);
    handle.cancel(Some(maho_ai::utils::abort::AbortReason::new(
        "Error",
        "caller cancelled",
    )));

    let result = bounded(fixture.lane.drive(drive_options("run"), &context)).await;
    let Err(DriveOptionsError::Cancelled(reason)) = result else {
        panic!("cancelled caller must reject, got {result:?}")
    };
    assert_eq!(reason.message, "caller cancelled");
    assert!(fixture.lane.active_drive_operation_id().is_none());
    assert_eq!(fixture.faux.state().call_count, 0);
}

#[tokio::test]
async fn caller_cancellation_stops_only_that_callers_observation() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let (answer, started, release) = gated_answer("answer");
    fixture.faux.set_responses(vec![answer]);
    accept_run(&fixture.lane, "run").await;
    let owner_lane = fixture.lane.clone();
    let owner = tokio::spawn(async move { drive(&owner_lane, "run").await });
    bounded(started).await.expect("provider started");
    let (context, handle) = with_cancel(&BACKGROUND_CONTEXT);
    let observer_lane = fixture.lane.clone();
    let observer =
        tokio::spawn(async move { observer_lane.drive(drive_options("run"), &context).await });
    handle.cancel(Some(maho_ai::utils::abort::AbortReason::new(
        "Error",
        "observer cancelled",
    )));

    let observed = bounded(observer).await.expect("observer task");
    let Err(DriveOptionsError::Cancelled(reason)) = observed else {
        panic!("observer must reject with its cancellation, got {observed:?}")
    };
    assert_eq!(reason.message, "observer cancelled");
    assert_eq!(
        fixture.lane.active_drive_operation_id().as_deref(),
        Some("run")
    );
    release.send(()).expect("release provider");
    let owned = bounded(owner)
        .await
        .expect("owner task")
        .expect("owner drive");
    assert!(matches!(owned, DriveOutcome::Settled { .. }));
    assert_eq!(fixture.faux.state().call_count, 1);
}

#[tokio::test]
async fn close_rejects_observation_without_waiting_for_a_non_cooperative_effect() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let (answer, started, release) = gated_answer("late");
    fixture.faux.set_responses(vec![answer]);
    accept_run(&fixture.lane, "run").await;
    let observer_lane = fixture.lane.clone();
    let observation = tokio::spawn(async move { drive(&observer_lane, "run").await });
    bounded(started).await.expect("provider started");

    bounded(fixture.harness.close(&BACKGROUND_CONTEXT)).await;
    let observed = bounded(observation).await.expect("observation task");
    let Err(DriveOptionsError::Drive(message)) = observed else {
        panic!("close must reject the observation, got {observed:?}")
    };
    assert_eq!(message, maho_agent::harness::result::HarnessClosed::MESSAGE);
    let _ = release.send(());
    // Pinned `expect.poll(() => activeDrive).toBeUndefined()`: the detached pass clears its
    // installation once the non-cooperative effect returns; the whole wait is bounded and each
    // notification is registered before the check it guards.
    bounded(async {
        loop {
            let changed = fixture.lane.state_changed();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if fixture.lane.active_drive_operation_id().is_none() {
                break;
            }
            changed.await;
        }
    })
    .await;
}

#[tokio::test]
async fn exposes_durable_abort_and_reconciles_through_public_drive() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_run(&fixture.lane, "run").await;

    let requested = fixture
        .lane
        .request_operation_abort("run".into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort requested");
    assert_eq!(requested.operation_id, "run");
    assert!(requested.newly_requested);
    assert!(requested.steer.is_empty());
    assert!(requested.follow_up.is_empty());
    assert!(fixture.lane.active_drive_operation_id().is_none());
    let outcome = drive(&fixture.lane, "run").await.expect("drive");
    assert_eq!(settled(&outcome).operation_id, "run");
    assert_eq!(settled(&outcome).status, TerminalStatus::Aborted);
    assert_eq!(fixture.faux.state().call_count, 0);
    let mismatch = fixture
        .lane
        .request_operation_abort("run".into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect_err("settled abort mismatches");
    assert_eq!(mismatch.expected, "run");
    assert_eq!(mismatch.last_operation_id.as_deref(), Some("run"));
}

#[tokio::test]
async fn faults_the_harness_when_a_detached_pass_fails() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let _registration = fixture.harness.hooks.on(
        HookName::BeforeDrive,
        Arc::new(|_, _| Box::pin(async { Err("drive failed".to_owned()) })),
        None,
    );
    accept_run(&fixture.lane, "run").await;

    let result = drive(&fixture.lane, "run").await;
    let Err(DriveOptionsError::Drive(message)) = result else {
        panic!("a failed detached pass must reject, got {result:?}")
    };
    assert!(
        message.contains("AgentHarness storage or invariant fault"),
        "{message}"
    );
    assert!(fixture.lane.active_drive_operation_id().is_none());
    let error = fixture
        .lane
        .get_tip_id()
        .expect_err("faulted lane rejects reads");
    assert!(fixture.lane.faulted());
    assert_ne!(error.kind, SessionErrorKind::Closed);
}
