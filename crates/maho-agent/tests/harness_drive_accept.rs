use std::sync::Arc;
use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::harness::create_agent_harness;
use maho_agent::harness::runtime::lane::{AdmissionError, Lane, PromptInput, QueuedInput};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions};
use maho_agent::harness::session::types::*;

async fn fixture() -> Result<Arc<Lane>, maho_agent::harness::session::session::SessionError> {
    let session = Arc::new(StorageBackedSession::new(SessionMetadata { id: "accept".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None }, Arc::new(MemoryStorage::new(MemoryStorageOptions::default())), StorageBackedSessionOptions::default()));
    session.attach();
    create_agent_harness(session, LaneConfiguration { model: LaneModelRef { provider: "test".into(), model_id: "model".into() }, thinking_level: maho_ai::types::ModelThinkingLevel::Off, active_tool_names: vec![] }, &BACKGROUND_CONTEXT).await?.lane("main", None, &BACKGROUND_CONTEXT).await
}

fn settings() -> RunSettings {
    RunSettings { compaction: maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS, steering_mode: maho_agent::types::QueueMode::All, follow_up_mode: maho_agent::types::QueueMode::All, tool_execution: ToolExecutionMode::Parallel }
}

async fn normalized(text: &str, images: Vec<maho_ai::types::ImageContent>, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let lane = fixture().await?;
    let admission = lane.accept_prompt(PromptInput::Text { text: text.into(), images }, Some("op".into()), settings(), &BACKGROUND_CONTEXT).await??;
    assert_eq!(admission.operation_id, "op");
    let state = lane.state();
    let OperationState::Starting(start) = state.operation.ok_or("missing operation")?.state else { panic!("starting"); };
    assert_eq!(start.operation.settings, settings());
    let entry = lane.session.get_entry(&state.tip_id.ok_or("missing tip")?, &BACKGROUND_CONTEXT).await?.ok_or("missing entry")?;
    let EntryKind::Message { message, .. } = entry.kind else { panic!("message"); };
    let Some(maho_ai::types::Message::User(user)) = message.try_as_llm() else { panic!("user"); };
    let maho_ai::types::UserContent::Blocks(blocks) = &user.content else { panic!("blocks"); };
    assert_eq!(blocks.len(), count);
    Ok(())
}

fn image() -> maho_ai::types::ImageContent { maho_ai::types::ImageContent { data: "aW1hZ2U=".into(), mime_type: "image/png".into() } }

#[tokio::test]
async fn accepts_normalized_text() { normalized("hello", vec![], 1).await.unwrap(); }
#[tokio::test]
async fn accepts_normalized_images() { normalized("", vec![image()], 1).await.unwrap(); }
#[tokio::test]
async fn accepts_normalized_text_and_images() { normalized("hello", vec![image()], 2).await.unwrap(); }

#[tokio::test]
async fn captures_next_run_before_empty_prompt() {
    let lane = fixture().await.unwrap();
    let first = lane.next_run(QueuedInput::Text("first".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let second = lane.next_run(QueuedInput::Text("second".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    lane.accept_prompt(PromptInput::Text { text: "".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert_eq!(lane.get_tip_id().unwrap(), Some(second.clone()));
    assert!(lane.state().inbox.is_empty());
    let entry = lane.session.get_entry(&second, &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert_eq!(entry.parent_id, Some(first.clone()));
    for id in [first, second] { assert!(lane.session.get_value(&maho_agent::harness::session::values::pending_entry(&id), &BACKGROUND_CONTEXT).await.unwrap().is_none()); }
}

#[tokio::test]
async fn rejects_empty_without_faulting_lane() {
    let lane = fixture().await.unwrap();
    assert_eq!(lane.accept_prompt(PromptInput::Messages(vec![]), None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::Empty));
    assert!(lane.state().operation.is_none());
    assert!(lane.get_tip_id().unwrap().is_none());
}

#[tokio::test]
async fn rejects_busy_without_overwriting_operation() {
    let lane = fixture().await.unwrap();
    lane.accept_prompt(PromptInput::Text { text: "one".into(), images: vec![] }, Some("first".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let tip = lane.get_tip_id().unwrap();
    assert_eq!(lane.accept_prompt(PromptInput::Text { text: "two".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::LaneBusy { lane: "main".into(), operation_id: "first".into() }));
    assert_eq!(lane.get_tip_id().unwrap(), tip);
}

#[tokio::test]
async fn preserves_message_array_and_prompt_entry_ids() {
    let lane = fixture().await.unwrap();
    let messages: Vec<_> = [10, 11].into_iter().map(|timestamp| maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage { content: maho_ai::types::UserContent::Text(timestamp.to_string()), timestamp }))).collect();
    lane.accept_prompt(PromptInput::Messages(messages.clone()), Some("operation".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let OperationIntent::Run { prompt_entry_ids } = lane.state().operation.unwrap().meta.intent else { panic!("run"); };
    assert_eq!(prompt_entry_ids.len(), 2);
    for (id, expected) in prompt_entry_ids.iter().zip(messages) {
        let entry = lane.session.get_entry(id, &BACKGROUND_CONTEXT).await.unwrap().unwrap();
        let EntryKind::Message { message, .. } = entry.kind else { panic!("message"); };
        assert_eq!(message, expected);
    }
}

#[tokio::test]
async fn concurrent_accepts_have_exactly_one_winner() {
    let lane = fixture().await.unwrap();
    let (left, right) = tokio::join!(
        lane.accept_prompt(PromptInput::Text { text: "first".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT),
        lane.accept_prompt(PromptInput::Text { text: "second".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT)
    );
    let outcomes = [left.unwrap(), right.unwrap()];
    assert_eq!(outcomes.iter().filter(|value| value.is_ok()).count(), 1);
    assert_eq!(outcomes.iter().filter(|value| matches!(value, Err(AdmissionError::LaneBusy { .. }))).count(), 1);
}

#[tokio::test]
async fn captures_eligible_tags_in_order_and_leaves_second_items() {
    let lane = fixture().await.unwrap();
    let first = lane.steer(QueuedInput::Text("first".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let next = lane.next_run(QueuedInput::Text("next".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let follow = lane.follow_up(QueuedInput::Text("follow".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let second = lane.steer(QueuedInput::Text("second".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let second_follow = lane.follow_up(QueuedInput::Text("second follow".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let mut modes = settings();
    modes.steering_mode = maho_agent::types::QueueMode::OneAtATime;
    modes.follow_up_mode = maho_agent::types::QueueMode::OneAtATime;
    lane.accept_prompt(PromptInput::Text { text: "request".into(), images: vec![] }, None, modes, &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let state = lane.state();
    assert_eq!(state.inbox.iter().map(|item| item.entry_id.clone()).collect::<Vec<_>>(), vec![second, second_follow]);
    let request = lane.session.get_entry(&state.tip_id.unwrap(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert_eq!(request.parent_id, Some(follow.clone()));
    assert_eq!(lane.session.get_entry(&follow, &BACKGROUND_CONTEXT).await.unwrap().unwrap().parent_id, Some(next.clone()));
    assert_eq!(lane.session.get_entry(&next, &BACKGROUND_CONTEXT).await.unwrap().unwrap().parent_id, Some(first));
}

async fn gated_fixture() -> Result<(Arc<Lane>, Arc<maho_agent::harness::session::testing::GatingStorage>), maho_agent::harness::session::session::SessionError> {
    let storage = Arc::new(maho_agent::harness::session::testing::GatingStorage::new(Arc::new(MemoryStorage::new(MemoryStorageOptions::default()))));
    let session = Arc::new(StorageBackedSession::new(SessionMetadata { id: "gated".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None }, storage.clone(), StorageBackedSessionOptions::default()));
    session.attach();
    let harness = create_agent_harness(session, LaneConfiguration { model: LaneModelRef { provider: "test".into(), model_id: "model".into() }, thinking_level: maho_ai::types::ModelThinkingLevel::Off, active_tool_names: vec![] }, &BACKGROUND_CONTEXT).await?;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await?;
    storage.arm();
    Ok((lane, storage))
}

#[tokio::test]
async fn failed_commit_does_not_publish_acceptance() {
    let (lane, storage) = gated_fixture().await.unwrap();
    let writer = lane.clone();
    let accept = tokio::spawn(async move { writer.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    storage.discard();
    assert!(accept.await.unwrap().is_err());
    assert!(lane.state().operation.is_none());
    assert!(lane.state().tip_id.is_none());
}

#[tokio::test]
async fn admitted_acceptance_finishes_after_sealing() {
    let (lane, storage) = gated_fixture().await.unwrap();
    let writer = lane.clone();
    let accept = tokio::spawn(async move { writer.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    lane.seal(maho_agent::harness::session::session::SessionError::new(maho_agent::harness::session::session::SessionErrorKind::Closed, "closed"));
    storage.next(1).await.unwrap();
    assert!(accept.await.unwrap().unwrap().is_ok());
    assert!(lane.state().operation.is_some());
    assert!(lane.get_tip_id().is_err());
}

#[tokio::test]
async fn acceptance_publishes_memory_only_after_commit() {
    let (lane, storage) = gated_fixture().await.unwrap();
    let writer = lane.clone();
    let accept = tokio::spawn(async move { writer.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    assert!(lane.state().operation.is_none());
    assert!(lane.state().tip_id.is_none());
    storage.next(1).await.unwrap();
    accept.await.unwrap().unwrap().unwrap();
    assert!(lane.state().operation.is_some());
}

#[tokio::test]
async fn durable_abort_removes_conversation_queues_but_preserves_next_run() {
    let lane = fixture().await.unwrap();
    lane.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, Some("op".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let steer = lane.steer(QueuedInput::Text("steer".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let follow = lane.follow_up(QueuedInput::Text("follow".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let next = lane.next_run(QueuedInput::Text("next".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    let result = lane.request_operation_abort("op".into(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert!(result.newly_requested);
    assert_eq!(result.steer.len(), 1);
    assert_eq!(result.follow_up.len(), 1);
    assert_eq!(lane.state().inbox, vec![InboxItem { entry_id: next, kind: InboxItemKind::NextRun }]);
    for id in [steer, follow] { assert!(lane.session.get_value(&maho_agent::harness::session::values::pending_entry(&id), &BACKGROUND_CONTEXT).await.unwrap().is_none()); }
    assert!(matches!(lane.state().operation.unwrap().state.operation_scope_of().control, Control::CancelRequested { .. }));
    let repeat = lane.request_operation_abort("op".into(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert!(!repeat.newly_requested);
    assert!(repeat.steer.is_empty());
}

#[tokio::test]
async fn stale_abort_does_not_change_current_operation() {
    let lane = fixture().await.unwrap();
    lane.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, Some("current".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let mismatch = lane.request_operation_abort("stale".into(), &BACKGROUND_CONTEXT).await.unwrap().unwrap_err();
    assert_eq!(mismatch.current_operation_id.as_deref(), Some("current"));
    assert!(matches!(lane.state().operation.unwrap().state.operation_scope_of().control, Control::Running));
}

#[tokio::test]
async fn cancellation_diverts_continuation_but_allows_settlement() {
    use maho_agent::harness::runtime::lane::{ContinueOperationResult, OperationCommand};
    let lane = fixture().await.unwrap();
    lane.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, Some("op".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    lane.request_operation_abort("op".into(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let continued = lane.continue_operation::<(), _>(|_, _, _, _| Box::pin(async { panic!("cancelled continuation must not plan") }), &BACKGROUND_CONTEXT).await.unwrap();
    assert!(matches!(continued, ContinueOperationResult::CancelRequested));
    lane.settle_operation(|_, current, _, _| Box::pin(async move { assert!(matches!(current.operation_scope_of().control, Control::CancelRequested { .. })); Ok(OperationCommand::Return { result: () }) }), &BACKGROUND_CONTEXT).await.unwrap();
}

#[tokio::test]
async fn execution_inspection_reports_configured_and_current_operation() {
    let lane = fixture().await.unwrap();
    let idle = lane.inspect_execution(&BACKGROUND_CONTEXT).await.unwrap();
    assert!(idle.current.is_none());
    assert_eq!(idle.configured_model.model_id, "model");
    lane.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, Some("op".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let open = lane.inspect_execution(&BACKGROUND_CONTEXT).await.unwrap();
    let current = open.current.unwrap();
    assert_eq!(current.id, "op");
    assert!(current.captured_model.is_none());
    assert_eq!(current.status, maho_agent::harness::agent_harness::OperationStatus::Open);
    lane.request_operation_abort("op".into(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let aborting = lane.inspect_execution(&BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(aborting.current.unwrap().status, maho_agent::harness::agent_harness::OperationStatus::Aborting);
}

#[tokio::test]
async fn accepts_standalone_compaction_with_durable_preparation() {
    let lane = fixture().await.unwrap();
    let message = maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage { content: maho_ai::types::UserContent::Text("history".into()), timestamp: 1 }));
    let tip = lane.append_message(message, &BACKGROUND_CONTEXT).await.unwrap();
    let admission = lane.accept_compaction(Some("focus".into()), Some("compact".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert_eq!(admission.kind, OperationIntentKind::Compaction);
    assert_eq!(lane.get_tip_id().unwrap(), Some(tip));
    let OperationState::SummaryDeciding(current) = lane.state().operation.unwrap().state else { panic!("summary deciding"); };
    assert_eq!(current.task.reason, Some(SummaryTaskReason::Manual));
    assert_eq!(current.task.custom_instructions.as_deref(), Some("focus"));
    let prepared = lane.session.get_value(&maho_agent::harness::session::values::operation_preparation("compact", &current.task.task_id), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert_eq!(prepared.value["kind"], "compaction");
}

#[tokio::test]
async fn rejects_empty_standalone_compaction_without_operation() {
    let lane = fixture().await.unwrap();
    assert_eq!(lane.accept_compaction(None, None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::NothingToCompact));
    assert!(lane.state().operation.is_none());
    assert!(lane.get_tip_id().unwrap().is_none());
}

async fn navigation_fixture(summarize: bool) -> Result<Arc<Lane>, Box<dyn std::error::Error>> {
    let lane = fixture().await?;
    let root = lane.append_custom_entry("root".into(), None, &BACKGROUND_CONTEXT).await?;
    let source = lane.append_custom_entry("source".into(), None, &BACKGROUND_CONTEXT).await?;
    let mutation = lane.session.begin_mutation(&BACKGROUND_CONTEXT).await?;
    mutation.commit(vec![maho_agent::harness::session::commit::insert_entry(NewEntry::custom("target", Some(root), "target"))], &BACKGROUND_CONTEXT).await?;
    mutation.end(&BACKGROUND_CONTEXT).await;
    lane.accept_navigation(Some("target".into()), maho_agent::harness::runtime::lane::NavigationOptions { summarize, label: Some("label".into()), custom_instructions: None }, Some("nav".into()), settings(), &BACKGROUND_CONTEXT).await??;
    assert_eq!(lane.get_tip_id()?, Some(source));
    Ok(lane)
}

#[tokio::test]
async fn accepts_unsummarized_navigation_without_moving_tip() {
    let lane = navigation_fixture(false).await.unwrap();
    let OperationState::NavigationReadyToCommit(current) = lane.state().operation.unwrap().state else { panic!("navigation ready"); };
    assert_eq!(current.target_id.as_deref(), Some("target"));
    assert_eq!(current.label.as_deref(), Some("label"));
}

#[tokio::test]
async fn accepts_summarized_navigation_with_durable_preparation() {
    let lane = navigation_fixture(true).await.unwrap();
    let OperationState::SummaryDeciding(current) = lane.state().operation.unwrap().state else { panic!("summary deciding"); };
    let stored = lane.session.get_value(&maho_agent::harness::session::values::operation_preparation("nav", &current.task.task_id), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    assert_eq!(stored.value["kind"], "branch_summary");
    assert!(matches!(current.task.boundary, ResultBoundary::CommitNavigation { .. }));
}

#[tokio::test]
async fn navigation_rejections_do_not_install_operation() {
    let lane = fixture().await.unwrap();
    let source = lane.append_custom_entry("source".into(), None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(lane.accept_navigation(Some(source.clone()), Default::default(), None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::InvalidNavigation { reason: "current_tip" }));
    assert_eq!(lane.accept_navigation(None, maho_agent::harness::runtime::lane::NavigationOptions { label: Some("bad".into()), ..Default::default() }, None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::InvalidNavigation { reason: "root_label" }));
    assert_eq!(lane.accept_navigation(Some("missing".into()), Default::default(), None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::UnknownTarget { target_id: "missing".into() }));
    assert!(lane.state().operation.is_none());
    assert_eq!(lane.get_tip_id().unwrap(), Some(source));
}

#[tokio::test]
async fn navigation_terminal_transaction_moves_tip_and_cleans_operation() {
    let lane = navigation_fixture(false).await.unwrap();
    let drive = maho_agent::harness::runtime::types::Drive::new(&maho_agent::harness::agent_harness::DriveOptions { operation_id: "nav".into(), wait_for_retry: None, poll_deferred: None }, &BACKGROUND_CONTEXT);
    let result = maho_agent::harness::runtime::structural::commit_navigation(&lane, &drive).await.unwrap();
    assert!(matches!(result, maho_agent::harness::runtime::types::ProcedureResult::Settled { .. }));
    assert_eq!(lane.get_tip_id().unwrap().as_deref(), Some("target"));
    assert!(lane.state().operation.is_none());
    assert_eq!(lane.state().last_operation_id.as_deref(), Some("nav"));
    assert_eq!(lane.session.get_label("target", &BACKGROUND_CONTEXT).await.unwrap().as_deref(), Some("label"));
    assert!(lane.session.get_value(&maho_agent::harness::session::values::operation_meta("nav"), &BACKGROUND_CONTEXT).await.unwrap().is_none());
    assert!(lane.session.get_value(&maho_agent::harness::session::values::operation_state("nav"), &BACKGROUND_CONTEXT).await.unwrap().is_none());
    assert_eq!(lane.get_result("nav", &BACKGROUND_CONTEXT).await.unwrap().unwrap().status, TerminalStatus::Completed);
}

#[tokio::test]
async fn cancelled_navigation_does_not_move_tip() {
    let lane = navigation_fixture(false).await.unwrap();
    let tip = lane.get_tip_id().unwrap();
    lane.request_operation_abort("nav".into(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let drive = maho_agent::harness::runtime::types::Drive::new(&maho_agent::harness::agent_harness::DriveOptions { operation_id: "nav".into(), wait_for_retry: None, poll_deferred: None }, &BACKGROUND_CONTEXT);
    assert_eq!(maho_agent::harness::runtime::structural::commit_navigation(&lane, &drive).await.unwrap(), maho_agent::harness::runtime::types::ProcedureResult::Continue);
    assert_eq!(lane.get_tip_id().unwrap(), tip);
}

#[tokio::test]
async fn acceptance_listener_reads_committed_execution_without_deadlock() {
    let lane = fixture().await.unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(std::sync::Mutex::new(Some(sender)));
    let reader = lane.clone();
    let _subscription = lane.events.on("run_start", Arc::new(move |_, context| {
        let reader = reader.clone();
        let sender = sender.clone();
        Box::pin(async move {
            let observation = reader.inspect_execution(&context).await;
            if let Some(sender) = sender.lock().unwrap().take() { let _ = sender.send(observation); }
        })
    }));
    lane.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, Some("op".into()), settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    let observed = tokio::time::timeout(std::time::Duration::from_secs(2), receiver).await.unwrap().unwrap().unwrap();
    assert_eq!(observed.current.unwrap().id, "op");
}

#[tokio::test]
async fn acceptance_waits_for_direct_listener_completion() {
    let lane = fixture().await.unwrap();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let started = Arc::new(std::sync::Mutex::new(Some(started_sender)));
    let release = Arc::new(std::sync::Mutex::new(Some(release_receiver)));
    let _subscription = lane.events.on("run_start", Arc::new(move |_, _| {
        let started = started.clone();
        let release = release.clone();
        Box::pin(async move {
            if let Some(sender) = started.lock().unwrap().take() { let _ = sender.send(()); }
            let receiver = release.lock().unwrap().take();
            if let Some(receiver) = receiver { receiver.await.unwrap(); }
        })
    }));
    let writer = lane.clone();
    let acceptance = tokio::spawn(async move { writer.accept_prompt(PromptInput::Text { text: "hello".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), started_receiver).await.unwrap().unwrap();
    assert!(!acceptance.is_finished());
    release_sender.send(()).unwrap();
    acceptance.await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn idle_callback_excludes_acceptance_and_allows_execution_reads() {
    let lane = fixture().await.unwrap();
    let (started, observed) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let reader = lane.clone();
    let owner = lane.clone();
    let callback = tokio::spawn(async move { owner.run_when_idle(move |context| async move {
        assert!(reader.inspect_execution(&context).await?.current.is_none());
        started.send(()).unwrap();
        released.await.unwrap();
        Ok(())
    }, &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), observed).await.unwrap().unwrap();
    let input = lane.accept_prompt(PromptInput::Text { text: "after".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT);
    tokio::pin!(input);
    assert!(futures::poll!(input.as_mut()).is_pending());
    release.send(()).unwrap();
    callback.await.unwrap().unwrap();
    input.await.unwrap().unwrap();
}

#[tokio::test]
async fn idle_callbacks_serialize_in_admission_order() {
    let lane = fixture().await.unwrap();
    let (started, observed) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let first_order = order.clone();
    let owner = lane.clone();
    let first = tokio::spawn(async move { owner.run_when_idle(move |_| async move {
        first_order.lock().unwrap().push("first:start");
        started.send(()).unwrap();
        released.await.unwrap();
        first_order.lock().unwrap().push("first:end");
        Ok(())
    }, &BACKGROUND_CONTEXT).await });
    observed.await.unwrap();
    let second_order = order.clone();
    let second = lane.run_when_idle(move |_| async move { second_order.lock().unwrap().push("second"); Ok(()) }, &BACKGROUND_CONTEXT);
    tokio::pin!(second);
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert_eq!(*order.lock().unwrap(), vec!["first:start"]);
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["first:start", "first:end", "second"]);
}

#[tokio::test]
async fn failing_idle_callback_releases_ownership() {
    let lane = fixture().await.unwrap();
    let result = lane.run_when_idle(|_| async { Err(maho_agent::harness::session::session::session_invariant_error("callback failed")) }, &BACKGROUND_CONTEXT).await;
    assert!(result.is_err());
    lane.accept_prompt(PromptInput::Text { text: "after".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await.unwrap().unwrap();
}

#[tokio::test]
async fn wait_for_idle_tracks_operation_without_installed_drive() {
    let lane = navigation_fixture(false).await.unwrap();
    let waiting = lane.wait_for_idle(&BACKGROUND_CONTEXT);
    tokio::pin!(waiting);
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    let drive = maho_agent::harness::runtime::types::Drive::new(&maho_agent::harness::agent_harness::DriveOptions { operation_id: "nav".into(), wait_for_retry: None, poll_deferred: None }, &BACKGROUND_CONTEXT);
    let (settled, idle) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(maho_agent::harness::runtime::structural::commit_navigation(&lane, &drive), waiting)
    }).await.unwrap();
    settled.unwrap();
    idle.unwrap();
}

#[tokio::test]
async fn close_waits_for_admitted_idle_callback() {
    let session = Arc::new(StorageBackedSession::new(SessionMetadata { id: "closing-idle".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None }, Arc::new(MemoryStorage::new(MemoryStorageOptions::default())), StorageBackedSessionOptions::default()));
    session.attach();
    let harness = create_agent_harness(session, LaneConfiguration { model: LaneModelRef { provider: "test".into(), model_id: "model".into() }, thinking_level: maho_ai::types::ModelThinkingLevel::Off, active_tool_names: vec![] }, &BACKGROUND_CONTEXT).await.unwrap();
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let (started, observed) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let owner = lane.clone();
    let callback = tokio::spawn(async move { owner.run_when_idle(move |_| async move { started.send(()).unwrap(); released.await.unwrap(); Ok(()) }, &BACKGROUND_CONTEXT).await });
    observed.await.unwrap();
    let closing = harness.close(&BACKGROUND_CONTEXT);
    tokio::pin!(closing);
    assert!(futures::poll!(closing.as_mut()).is_pending());
    release.send(()).unwrap();
    callback.await.unwrap().unwrap();
    closing.await;
    assert!(lane.get_tip_id().is_err());
}

#[tokio::test]
async fn acceptance_after_close_returns_expected_closed_rejection() {
    let lane = fixture().await.unwrap();
    lane.seal(maho_agent::harness::session::session::SessionError::new(maho_agent::harness::session::session::SessionErrorKind::Closed, "AgentHarness is closed"));
    assert_eq!(lane.accept_prompt(PromptInput::Text { text: "late".into(), images: vec![] }, None, settings(), &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::Closed { message: "AgentHarness is closed".into() }));
    assert!(lane.state().operation.is_none());
}
