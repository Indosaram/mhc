//! Port of senpi packages/agent/test/harness/context.test.ts and the session-context cases of
//! senpi packages/agent/test/harness/compaction.test.ts.

mod support;

use maho_agent::harness::context::{BACKGROUND_CONTEXT, TODO_CONTEXT, get_telemetry_context, with_telemetry_context};
use maho_agent::harness::messages::{BRANCH_SUMMARY_PREFIX, COMPACTION_SUMMARY_PREFIX, convert_to_llm};
use maho_agent::harness::session::context::{build_context_entries, build_session_context, session_entry_to_context_messages};
use maho_agent::harness::session::types::{Entry, EntryKind, NewEntry};
use maho_agent::harness::telemetry::{InMemoryTelemetryContext, SpanOptions, TelemetryContext, noop_telemetry_context};
use maho_agent::types::{AgentMessage, CustomAgentMessage};
use maho_ai::types::{ContentBlock, Message, StopReason, UserContent, UserMessage};
use serde_json::Map;

fn user_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(text)]),
        timestamp,
    }))
}

fn message_entry(id: &str, parent_id: Option<&str>, message: AgentMessage, seq: i64) -> Entry {
    let mut entry = NewEntry::message(id, parent_id.map(str::to_owned), message);
    entry.id = id.to_owned();
    Entry {
        id: entry.id,
        parent_id: entry.parent_id,
        seq,
        timestamp: seq,
        kind: entry.kind,
    }
}

fn compaction_entry(id: &str, summary: &str, retained_tail: Vec<AgentMessage>, seq: i64) -> Entry {
    Entry {
        id: id.to_owned(),
        parent_id: None,
        seq,
        timestamp: seq,
        kind: EntryKind::Compaction {
            summary: summary.to_owned(),
            retained_tail,
            tokens_before: 1234,
            details: None,
            usage: None,
            from_hook: false,
        },
    }
}

#[test]
fn uses_no_op_telemetry_when_none_is_attached() {
    let noop = noop_telemetry_context();
    assert!(std::sync::Arc::ptr_eq(get_telemetry_context(&BACKGROUND_CONTEXT).backend(), noop.backend()));
    assert!(std::sync::Arc::ptr_eq(get_telemetry_context(&TODO_CONTEXT).backend(), noop.backend()));
}

#[tokio::test]
async fn carries_telemetry_as_an_ordinary_context_value() {
    let telemetry = InMemoryTelemetryContext::new();
    let context = with_telemetry_context(telemetry.context(), &BACKGROUND_CONTEXT);
    let parent_context = context.clone();
    get_telemetry_context(&context)
        .start_span(
            SpanOptions { name: "parent".to_owned(), attributes: Map::new() },
            move |span| {
                let child_context = with_telemetry_context(TelemetryContext::from_span(&span), &parent_context);
                Box::pin(async move {
                    get_telemetry_context(&child_context)
                        .start_span(
                            SpanOptions { name: "child".to_owned(), attributes: Map::new() },
                            |_span| Box::pin(async {}),
                        )
                        .await;
                })
            },
        )
        .await;
    let spans = telemetry.get_spans();
    assert_eq!(spans.iter().map(|span| span.name.as_str()).collect::<Vec<_>>(), vec!["parent", "child"]);
    assert_eq!(spans[1].parent_id, Some(spans[0].id));
}

#[test]
fn build_context_entries_keeps_the_newest_compaction_and_everything_after_it() {
    let entries = vec![
        message_entry("u1", None, user_message("one", 1), 1),
        compaction_entry("c1", "first", Vec::new(), 2),
        message_entry("u2", Some("c1"), user_message("two", 3), 3),
        compaction_entry("c2", "second", Vec::new(), 4),
        message_entry("u3", Some("c2"), user_message("three", 5), 5),
    ];
    let context = build_context_entries(&entries);
    assert_eq!(
        context.iter().map(|entry| entry.id.as_str()).collect::<Vec<&str>>(),
        vec!["c2", "u3"]
    );
}

#[tokio::test]
async fn build_session_context_projects_the_compaction_summary_first() {
    let entries = vec![
        message_entry("u1", None, user_message("one", 1), 1),
        compaction_entry("c1", "summary text", Vec::new(), 2),
        message_entry("u2", Some("c1"), user_message("two", 3), 3),
    ];
    let messages = build_session_context(&entries, None, &BACKGROUND_CONTEXT).await;
    assert_eq!(messages.len(), 2);
    match &messages[0] {
        AgentMessage::Custom(CustomAgentMessage::CompactionSummary(compaction)) => {
            assert_eq!(compaction.summary, "summary text");
        }
        other => panic!("expected compaction summary first, got {other:?}"),
    }
    let llm = convert_to_llm(messages);
    match &llm[0] {
        Message::User(user) => {
            let UserContent::Blocks(blocks) = &user.content else {
                panic!("expected blocks");
            };
            let ContentBlock::Text(text) = &blocks[0] else {
                panic!("expected text");
            };
            assert_eq!(text.text, format!("{COMPACTION_SUMMARY_PREFIX}summary text\n</summary>"));
        }
        other => panic!("expected user message, got {other:?}"),
    }
}

#[test]
fn drops_error_and_aborted_assistant_messages_from_context() {
    let mut assistant = maho_ai::utils::lazy::setup_error_message(&support::test_model(), "");
    assistant.stop_reason = StopReason::Error;
    let entry = Entry {
        id: "a1".to_owned(),
        parent_id: None,
        seq: 1,
        timestamp: 1,
        kind: EntryKind::Message {
            message: AgentMessage::Llm(Message::Assistant(Box::new(assistant))),
            terminate: None,
        },
    };
    assert!(session_entry_to_context_messages(&entry).is_empty());

    let branch = Entry {
        id: "b1".to_owned(),
        parent_id: None,
        seq: 2,
        timestamp: 2,
        kind: EntryKind::BranchSummary {
            from_id: None,
            summary: "branch text".to_owned(),
            details: None,
            usage: None,
            from_hook: false,
        },
    };
    let messages = session_entry_to_context_messages(&branch);
    let llm = convert_to_llm(messages);
    match &llm[0] {
        Message::User(user) => {
            let UserContent::Blocks(blocks) = &user.content else {
                panic!("expected blocks");
            };
            let ContentBlock::Text(text) = &blocks[0] else {
                panic!("expected text");
            };
            assert_eq!(text.text, format!("{BRANCH_SUMMARY_PREFIX}branch text</summary>"));
        }
        other => panic!("expected user message, got {other:?}"),
    }
}
