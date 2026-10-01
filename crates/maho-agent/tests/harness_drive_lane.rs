use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::harness::{Harness, create_agent_harness};
use maho_agent::harness::runtime::lane::{CommitDecision, LaneCommand};
use maho_agent::harness::session::types::{
    LaneConfiguration, LaneModelRef, Session, SessionMetadata, SessionReader, Write,
};
use maho_agent::harness::session::values::{lane_config, set_value};
use maho_agent::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_ai::types::ModelThinkingLevel;
use std::sync::Arc;

fn seed() -> LaneConfiguration {
    LaneConfiguration {
        model: LaneModelRef {
            provider: "test".into(),
            model_id: "model".into(),
        },
        thinking_level: ModelThinkingLevel::Off,
        active_tool_names: vec![],
    }
}

#[tokio::test]
async fn lane_admission_observes_current_harness_resources() {
    use maho_agent::harness::runtime::lane::AdmissionError;
    use maho_agent::harness::types::{AgentHarnessResources, Skill, PromptTemplate};
    let harness = fixture().await;
    let skill_lane = harness.lane("skill", None, &BACKGROUND_CONTEXT).await.unwrap();
    let template_lane = harness.lane("template", None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(skill_lane.accept_skill("review", None, None, &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::UnknownSkill { name: "review".into() }));
    assert_eq!(template_lane.accept_prompt_template("fix", vec![], None, &BACKGROUND_CONTEXT).await.unwrap(), Err(AdmissionError::UnknownTemplate { name: "fix".into() }));
    harness.set_resources(AgentHarnessResources { skills: Some(vec![Skill { name: "review".into(), description: "Review".into(), content: "Inspect".into(), file_path: "/skills/review/SKILL.md".into(), disable_model_invocation: None }]), prompt_templates: Some(vec![PromptTemplate { name: "fix".into(), description: None, content: "$1".into() }]) }, &BACKGROUND_CONTEXT).await.unwrap();
    harness.set_steering_mode(maho_agent::types::QueueMode::OneAtATime, &BACKGROUND_CONTEXT).await.unwrap();
    skill_lane.accept_skill("review", Some("strict".into()), None, &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    template_lane.accept_prompt_template("fix", vec!["argument".into()], None, &BACKGROUND_CONTEXT).await.unwrap().unwrap();
    for lane in [skill_lane, template_lane] {
        let operation = lane.state().operation.unwrap();
        assert_eq!(operation.state.operation_scope_of().settings.steering_mode, maho_agent::types::QueueMode::OneAtATime);
        assert!(matches!(operation.meta.intent, maho_agent::harness::session::types::OperationIntent::Run { .. }));
        assert!(lane.get_tip_id().unwrap().is_some());
    }
}

#[tokio::test]
async fn resolves_model_from_shared_registry_without_requiring_registration_for_set() {
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    assert!(lane.get_model().unwrap().is_none());
    let faux = maho_ai::providers::faux::faux_provider(Default::default());
    let model = faux.get_model(None).unwrap();
    lane.set_model(LaneModelRef { provider: model.provider.clone(), model_id: model.id.clone() }, &BACKGROUND_CONTEXT).await.unwrap();
    assert!(lane.get_model().unwrap().is_none());
    harness.models.set_provider(faux.provider);
    assert_eq!(lane.get_model().unwrap().unwrap().id, model.id);
}

#[tokio::test]
async fn pending_assistant_acceptance_is_expected_rejection() {
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let pending = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions { stop_reason: Some(maho_ai::types::StopReason::Pending), ..Default::default() });
    let settings = maho_agent::harness::session::types::RunSettings { compaction: maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS, steering_mode: maho_agent::types::QueueMode::All, follow_up_mode: maho_agent::types::QueueMode::All, tool_execution: maho_agent::harness::session::types::ToolExecutionMode::Parallel };
    let result = lane.accept_prompt(maho_agent::harness::runtime::lane::PromptInput::Messages(vec![maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(pending)))]), None, settings, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(result, Err(maho_agent::harness::runtime::lane::AdmissionError::PendingAssistant));
    assert!(lane.get_tip_id().unwrap().is_none());
}

async fn gated_lane() -> Result<(Arc<maho_agent::harness::runtime::lane::Lane>, Arc<maho_agent::harness::session::testing::GatingStorage>), maho_agent::harness::session::session::SessionError> {
    let storage = Arc::new(maho_agent::harness::session::testing::GatingStorage::new(Arc::new(MemoryStorage::new(MemoryStorageOptions::default()))));
    let session = Arc::new(StorageBackedSession::new(SessionMetadata { id: "gated-lane".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None }, storage.clone(), StorageBackedSessionOptions::default()));
    session.attach();
    let harness = create_agent_harness(session, seed(), &BACKGROUND_CONTEXT).await?;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await?;
    storage.arm();
    Ok((lane, storage))
}

#[tokio::test]
async fn publishes_configuration_memory_only_after_commit() {
    let (lane, storage) = gated_lane().await.unwrap();
    let writer = lane.clone();
    let command = tokio::spawn(async move { writer.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    assert_eq!(lane.state().configuration.thinking_level, ModelThinkingLevel::Off);
    storage.next(1).await.unwrap();
    command.await.unwrap().unwrap();
    assert_eq!(lane.state().configuration.thinking_level, ModelThinkingLevel::High);
    assert_eq!(lane.session.get_value(&lane_config("main"), &BACKGROUND_CONTEXT).await.unwrap().unwrap().value["thinkingLevel"], "high");
}

#[tokio::test]
async fn preserves_configuration_memory_when_commit_fails() {
    let (lane, storage) = gated_lane().await.unwrap();
    let writer = lane.clone();
    let command = tokio::spawn(async move { writer.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    storage.discard();
    assert!(command.await.unwrap().is_err());
    assert_eq!(lane.state().configuration.thinking_level, ModelThinkingLevel::Off);
}

#[tokio::test]
async fn sealing_rejects_new_work_but_admitted_configuration_finishes() {
    let (lane, storage) = gated_lane().await.unwrap();
    let writer = lane.clone();
    let command = tokio::spawn(async move { writer.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    lane.seal(maho_agent::harness::session::session::session_invariant_error("sealed"));
    assert!(lane.get_tip_id().is_err());
    assert!(lane.set_thinking_level(ModelThinkingLevel::Low, &BACKGROUND_CONTEXT).await.is_err());
    storage.next(1).await.unwrap();
    command.await.unwrap().unwrap();
    assert_eq!(lane.state().configuration.thinking_level, ModelThinkingLevel::High);
}

#[tokio::test]
async fn queued_commands_plan_from_latest_committed_memory() {
    let (lane, storage) = gated_lane().await.unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let first_observed = observed.clone();
    let first_lane = lane.clone();
    let first = tokio::spawn(async move { first_lane.command(move |mut state, _| Box::pin(async move {
        first_observed.lock().unwrap().push(state.configuration.thinking_level);
        state.configuration.thinking_level = ModelThinkingLevel::High;
        let value = serde_json::to_value(&state.configuration).unwrap();
        Ok(LaneCommand::Commit { decision: CommitDecision { writes: vec![Write::Value(set_value(&lane_config("main"), value))], materialize: Arc::new(|_| ()), events: None }, next: Box::new(state) })
    }), &BACKGROUND_CONTEXT).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), storage.wait_pending(1)).await.unwrap().unwrap();
    let second_observed = observed.clone();
    let second = lane.command(move |mut state, _| Box::pin(async move {
        second_observed.lock().unwrap().push(state.configuration.thinking_level);
        state.configuration.thinking_level = ModelThinkingLevel::Medium;
        let value = serde_json::to_value(&state.configuration).unwrap();
        Ok(LaneCommand::Commit { decision: CommitDecision { writes: vec![Write::Value(set_value(&lane_config("main"), value))], materialize: Arc::new(|_| ()), events: None }, next: Box::new(state) })
    }), &BACKGROUND_CONTEXT);
    tokio::pin!(second);
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert_eq!(*observed.lock().unwrap(), vec![ModelThinkingLevel::Off]);
    storage.next(1).await.unwrap();
    first.await.unwrap().unwrap();
    let release = storage.next(1);
    let (result, released) = tokio::time::timeout(std::time::Duration::from_secs(2), async { tokio::join!(second, release) }).await.unwrap();
    result.unwrap();
    released.unwrap();
    assert_eq!(*observed.lock().unwrap(), vec![ModelThinkingLevel::Off, ModelThinkingLevel::High]);
    assert_eq!(lane.state().configuration.thinking_level, ModelThinkingLevel::Medium);
}

#[tokio::test]
async fn global_configuration_events_include_previous_and_current_values() {
    let harness = fixture().await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = seen.clone();
    let _subscription = harness.events.on("config_update", Arc::new(move |event, _| {
        let events = events.clone();
        Box::pin(async move { events.lock().unwrap().push(event.payload); })
    }));
    harness.set_stream_options(maho_agent::harness::types::AgentHarnessStreamOptions { timeout_ms: Some(123), ..Default::default() }, &BACKGROUND_CONTEXT).await.unwrap();
    harness.set_steering_mode(maho_agent::types::QueueMode::OneAtATime, &BACKGROUND_CONTEXT).await.unwrap();
    let events = seen.lock().unwrap();
    assert_eq!(events.len(), 2);
    let maho_agent::harness::events::HarnessEventPayload::ConfigUpdate { property, previous, value } = &events[0] else { panic!("config update"); };
    assert_eq!(property, "streamOptions");
    assert_eq!(previous, &serde_json::json!({}));
    assert_eq!(value, &serde_json::json!({"timeoutMs":123}));
    let maho_agent::harness::events::HarnessEventPayload::ConfigUpdate { property, previous, value } = &events[1] else { panic!("config update"); };
    assert_eq!(property, "steeringMode");
    assert_eq!(previous, "all");
    assert_eq!(value, "one-at-a-time");
}

#[tokio::test]
async fn drive_completion_preserves_first_settlement_and_wakes_all_observers() {
    use maho_agent::harness::runtime::lane::Drive;
    use maho_agent::harness::agent_harness::{DriveOptions, DriveOutcome};
    let drive = Drive::new(&DriveOptions { operation_id: "run".into(), wait_for_retry: Some(true), poll_deferred: Some(true) }, &BACKGROUND_CONTEXT);
    let outcome = DriveOutcome::WaitingRetry { operation_id: "run".into(), not_before: 10 };
    drive.settle(outcome.clone());
    drive.fail("late failure".into());
    let (left, right) = tokio::join!(drive.completion.wait(), drive.completion.wait());
    assert_eq!(left.unwrap(), outcome);
    assert_eq!(right.unwrap(), outcome);
    assert!(drive.wait_for_retry);
    assert_eq!(*drive.deferred_permits.lock().unwrap(), 1);
}

#[tokio::test]
async fn drive_close_aborts_gate_and_rejects_completion() {
    use maho_agent::harness::runtime::lane::Drive;
    use maho_agent::harness::agent_harness::DriveOptions;
    let drive = Drive::new(&DriveOptions { operation_id: "run".into(), wait_for_retry: None, poll_deferred: None }, &BACKGROUND_CONTEXT);
    drive.close_gate("closed".into());
    assert!(drive.close_signal.aborted());
    assert_eq!(drive.completion.wait().await.unwrap_err(), "closed");
}

#[tokio::test]
async fn global_configuration_round_trips_and_rejects_invalid_values() {
    let harness = fixture().await;
    let options = maho_agent::harness::types::AgentHarnessStreamOptions { timeout_ms: Some(42), ..Default::default() };
    harness.set_stream_options(options.clone(), &BACKGROUND_CONTEXT).await.unwrap();
    harness.set_steering_mode(maho_agent::types::QueueMode::OneAtATime, &BACKGROUND_CONTEXT).await.unwrap();
    harness.set_follow_up_mode(maho_agent::types::QueueMode::OneAtATime, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(harness.get_stream_options().unwrap(), options);
    assert_eq!(harness.get_steering_mode().unwrap(), maho_agent::types::QueueMode::OneAtATime);
    assert_eq!(harness.get_follow_up_mode().unwrap(), maho_agent::types::QueueMode::OneAtATime);
    let mut retry = harness.get_retry_policy().unwrap();
    retry.base_delay_ms = u64::MAX;
    assert!(harness.set_retry_policy(retry, &BACKGROUND_CONTEXT).await.is_err());
    assert_eq!(harness.get_retry_policy().unwrap().base_delay_ms, 1_000);
    let mut compaction = harness.get_compaction_settings().unwrap();
    compaction.reserve_tokens = u64::MAX;
    assert!(harness.set_compaction_settings(compaction, &BACKGROUND_CONTEXT).await.is_err());
}

#[tokio::test]
async fn closes_every_lane_and_rejects_later_acquisition() {
    let harness = fixture().await;
    let main = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let other = harness.lane("other", None, &BACKGROUND_CONTEXT).await.unwrap();
    harness.close(&BACKGROUND_CONTEXT).await;
    assert!(main.get_tip_id().is_err());
    assert!(other.get_tip_id().is_err());
    assert!(harness.lane("late", None, &BACKGROUND_CONTEXT).await.is_err());
}

#[tokio::test]
async fn replaces_global_resources_without_changing_lane_configuration() {
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let resources = maho_agent::harness::types::AgentHarnessResources { prompt_templates: Some(vec![maho_agent::harness::types::PromptTemplate { name: "fix".into(), description: None, content: "$1".into() }]), skills: None };
    harness.set_resources(resources.clone(), &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(harness.get_resources().unwrap(), resources);
    assert_eq!(lane.state().configuration, seed());
}

#[tokio::test]
async fn fault_seals_all_lanes_and_preserves_first_fault() {
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let cause = maho_agent::harness::session::session::session_invariant_error("broken storage");
    let first = harness.fault(cause, &BACKGROUND_CONTEXT);
    let second = harness.fault(maho_agent::harness::session::session::session_invariant_error("later"), &BACKGROUND_CONTEXT);
    assert_eq!(first, second);
    assert_eq!(lane.get_tip_id().unwrap_err(), first);
    assert!(harness.get_stream_options().is_err());
    harness.close(&BACKGROUND_CONTEXT).await;
    harness.close(&BACKGROUND_CONTEXT).await;
    assert!(harness.session.get_stats(&BACKGROUND_CONTEXT).await.is_err());
}

#[tokio::test]
async fn queues_all_input_kinds_without_moving_tip_and_cancels_one() {
    use maho_agent::harness::runtime::lane::{QueuedInput, CancelQueuedOutcome};
    use maho_agent::harness::session::types::InboxItemKind;
    use maho_agent::harness::session::values::pending_entry;
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let steer = lane.steer(QueuedInput::Text("steer".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    lane.follow_up(QueuedInput::Text("follow".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    lane.next_run(QueuedInput::Text("next".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(lane.state().inbox.iter().map(|item| item.kind).collect::<Vec<_>>(), vec![InboxItemKind::Steer, InboxItemKind::FollowUp, InboxItemKind::NextRun]);
    assert_eq!(lane.get_tip_id().unwrap(), None);
    assert_eq!(lane.cancel_queued(steer.clone(), &BACKGROUND_CONTEXT).await.unwrap(), CancelQueuedOutcome::Cancelled);
    assert_eq!(lane.cancel_queued(steer.clone(), &BACKGROUND_CONTEXT).await.unwrap(), CancelQueuedOutcome::NotFound);
    assert!(harness.session.get_value(&pending_entry(&steer), &BACKGROUND_CONTEXT).await.unwrap().is_none());
    assert_eq!(lane.state().inbox.len(), 2);
}

#[tokio::test]
async fn rejects_empty_queued_text_without_faulting_lane() {
    use maho_agent::harness::runtime::lane::QueuedInput;
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(lane.steer(QueuedInput::Text(String::new()), vec![], &BACKGROUND_CONTEXT).await.unwrap_err().message, "Queued input must contain text or an image");
    lane.steer(QueuedInput::Text("valid".into()), vec![], &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(lane.state().inbox.len(), 1);
}

#[tokio::test]
async fn cancelling_committed_entry_reports_already_consumed() {
    use maho_agent::harness::runtime::lane::CancelQueuedOutcome;
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let id = lane.append_custom_entry("committed".into(), None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(lane.cancel_queued(id, &BACKGROUND_CONTEXT).await.unwrap(), CancelQueuedOutcome::AlreadyConsumed);
}

#[tokio::test]
async fn records_adjustment_usage_without_changing_branch_tip() {
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let usage = maho_ai::types::Usage { input: 7, total_tokens: 7, ..Default::default() };
    let id = lane.record_usage(usage, None, Some(serde_json::json!({"reason": "external"})), &BACKGROUND_CONTEXT).await.unwrap();
    assert!(!id.is_empty());
    assert_eq!(harness.session.get_stats(&BACKGROUND_CONTEXT).await.unwrap().usage.input, 7);
    assert_eq!(lane.get_tip_id().unwrap(), None);
}

#[tokio::test]
async fn appends_custom_entries_in_a_parent_chain_and_reads_latest() {
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    let first = lane.append_custom_entry("first".into(), Some(serde_json::json!(1)), &BACKGROUND_CONTEXT).await.unwrap();
    let second = lane.append_custom_entry("second".into(), None, &BACKGROUND_CONTEXT).await.unwrap();
    let entries = lane.find_entries(None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, second);
    assert_eq!(entries[0].parent_id.as_ref(), Some(&first));
    assert_eq!(entries[1].parent_id, None);
    assert_eq!(lane.find_entry(None, &BACKGROUND_CONTEXT).await.unwrap().unwrap().id, second);
    assert_eq!(lane.get_tip_id().unwrap(), Some(second));
}

#[tokio::test]
async fn queued_writes_flush_before_idle_append_in_one_parent_chain() {
    use maho_agent::harness::session::types::{InboxItem, InboxItemKind, PendingEntry};
    use maho_agent::harness::session::values::pending_entry;
    let harness = fixture().await;
    let lane = harness.lane("main", None, &BACKGROUND_CONTEXT).await.unwrap();
    lane.command(|mut state, _| Box::pin(async move {
        state.inbox.push(InboxItem { entry_id: "queued".into(), kind: InboxItemKind::Write });
        Ok(LaneCommand::Commit { decision: CommitDecision { writes: vec![Write::Value(set_value(&pending_entry("queued"), serde_json::to_value(PendingEntry::Custom { custom_type: "queued".into(), payload: None }).unwrap()))], materialize: Arc::new(|_| ()), events: None }, next: Box::new(state) })
    }), &BACKGROUND_CONTEXT).await.unwrap();
    let id = lane.append_custom_entry("new".into(), None, &BACKGROUND_CONTEXT).await.unwrap();
    let entries = lane.find_entries(None, &BACKGROUND_CONTEXT).await.unwrap();
    assert_eq!(entries[0].id, id);
    assert_eq!(entries[0].parent_id.as_deref(), Some("queued"));
    assert!(lane.state().inbox.is_empty());
    assert!(harness.session.get_value(&pending_entry("queued"), &BACKGROUND_CONTEXT).await.unwrap().is_none());
    assert_eq!(entries[0].seq, entries[1].seq + 1);
}

async fn fixture() -> Harness {
    let storage = Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| 10)),
    }));
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "runtime".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
        },
        storage,
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    create_agent_harness(session, seed(), &BACKGROUND_CONTEXT)
        .await
        .expect("fresh harness attaches")
}

#[tokio::test]
async fn reads_and_replaces_configuration_from_owned_state() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    lane.set_model(
        LaneModelRef {
            provider: "test".into(),
            model_id: "updated".into(),
        },
        &BACKGROUND_CONTEXT,
    )
    .await
    .unwrap();
    lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    lane.set_active_tools(vec!["read".into()], &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(lane.get_thinking_level().unwrap(), ModelThinkingLevel::High);
    assert_eq!(lane.get_active_tools().unwrap(), vec!["read"]);
    let stored = harness
        .session
        .get_value(&lane_config("main"), &BACKGROUND_CONTEXT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.value,
        serde_json::json!({ "model": { "provider": "test", "modelId": "updated" }, "thinkingLevel": "high", "activeToolNames": ["read"] })
    );
    harness.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn derives_concurrent_configuration_updates_from_latest_state() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let (model, thinking) = tokio::join!(
        lane.set_model(
            LaneModelRef {
                provider: "test".into(),
                model_id: "updated".into()
            },
            &BACKGROUND_CONTEXT
        ),
        lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
    );
    model.unwrap();
    thinking.unwrap();
    assert_eq!(lane.state().configuration.model.model_id, "updated");
    assert_eq!(
        lane.state().configuration.thinking_level,
        ModelThinkingLevel::High
    );
}

#[tokio::test]
async fn returns_future_value_without_holding_session_line() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let pending = lane
        .command(
            move |_, _| Box::pin(async move { Ok(LaneCommand::Return { result: receive }) }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(lane.get_thinking_level().unwrap(), ModelThinkingLevel::High);
    send.send(()).unwrap();
    pending.await.unwrap();
}

#[tokio::test]
async fn expected_rejection_does_not_fault_lane() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let result = lane
        .command::<(), _>(
            |_, _| {
                Box::pin(async {
                    Ok(LaneCommand::Reject {
                        error: "declined".into(),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await;
    assert_eq!(result.unwrap_err().message, "declined");
    assert_eq!(lane.get_tip_id().unwrap(), None);
    lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
}

#[tokio::test]
async fn passes_bounded_reads_and_commit_metadata_through_command() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let observer = lane.clone();
    let commit = lane
        .command(
            move |mut state, reader| {
                Box::pin(async move {
                    let stored = reader
                        .get_value(&lane_config("main"), &BACKGROUND_CONTEXT)
                        .await?
                        .unwrap();
                    assert_eq!(stored.value, serde_json::to_value(seed()).unwrap());
                    state.configuration.thinking_level = ModelThinkingLevel::High;
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes: vec![Write::Value(set_value(
                                &lane_config("main"),
                                serde_json::to_value(&state.configuration).unwrap(),
                            ))],
                            materialize: Arc::new(move |commit| {
                                assert_eq!(
                                    observer.state().configuration.thinking_level,
                                    ModelThinkingLevel::High
                                );
                                commit.clone()
                            }),
                            events: None,
                        },
                        next: Box::new(state),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    assert_eq!(commit.seqs.len(), 1);
    assert_eq!(commit.timestamp, 10);
}

#[tokio::test]
async fn sealing_rejects_later_reads_and_commands() {
    let harness = fixture().await;
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    harness.close(&BACKGROUND_CONTEXT).await;
    assert!(lane.get_tip_id().is_err());
    assert!(
        lane.set_thinking_level(ModelThinkingLevel::High, &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn attaches_without_implicit_main_lane() {
    let harness = fixture().await;
    assert!(harness.lanes().unwrap().is_empty());
    assert!(
        harness
            .session
            .branch("main", &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn atomically_acquires_one_lane_under_concurrency() {
    let harness = fixture().await;
    let (left, right) = tokio::join!(
        harness.lane("main", None, &BACKGROUND_CONTEXT),
        harness.lane("main", None, &BACKGROUND_CONTEXT)
    );
    assert!(Arc::ptr_eq(&left.unwrap(), &right.unwrap()));
    assert_eq!(harness.lanes().unwrap().len(), 1);
}

#[tokio::test]
async fn validates_create_at_only_for_missing_lane() {
    let harness = fixture().await;
    assert!(
        harness
            .lane("missing", Some("unknown".into()), &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
    let first = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let second = harness
        .lane("main", Some("unknown".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(harness.lane("", None, &BACKGROUND_CONTEXT).await.is_err());
    assert!(
        harness
            .lane("bad\0name", None, &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn attaches_to_data_only_branch_without_moving_tip() {
    let harness = fixture().await;
    harness
        .session
        .create_branch("data", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let lane = harness
        .lane("data", Some("ignored".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(lane.get_tip_id().unwrap(), None);
    assert_eq!(lane.state().configuration, seed());
}

#[tokio::test]
async fn restores_complete_lanes_without_main() {
    let harness = fixture().await;
    harness
        .lane("other", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let restored = create_agent_harness(harness.session.clone(), seed(), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(restored.lanes().unwrap()[0].0, "other");
}

#[tokio::test]
async fn rejects_partial_durable_lane_state() {
    let harness = fixture().await;
    harness
        .session
        .set_value(
            &lane_config("partial"),
            serde_json::to_value(seed()).unwrap(),
            &BACKGROUND_CONTEXT,
        )
        .await
        .unwrap();
    assert!(
        create_agent_harness(harness.session.clone(), seed(), &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn persists_name_and_label_updates_and_deletions() {
    let harness = fixture().await;
    harness
        .set_name(Some("named".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    harness
        .set_label("entry", Some("label".into()), &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(
        harness
            .get_name(&BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .as_deref(),
        Some("named")
    );
    assert_eq!(
        harness
            .get_label("entry", &BACKGROUND_CONTEXT)
            .await
            .unwrap()
            .as_deref(),
        Some("label")
    );
    harness.set_name(None, &BACKGROUND_CONTEXT).await.unwrap();
    harness
        .set_label("entry", None, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(harness.get_name(&BACKGROUND_CONTEXT).await.unwrap(), None);
    assert_eq!(
        harness
            .get_label("entry", &BACKGROUND_CONTEXT)
            .await
            .unwrap(),
        None
    );
}
