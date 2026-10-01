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
