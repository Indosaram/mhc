//! Port of the pure-helper cases of senpi packages/agent/test/harness/compaction.test.ts.

use maho_agent::harness::compaction::compaction::{
    CompactionSettings, calculate_context_tokens, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, get_last_assistant_usage, prepare_compaction, serialize_conversation, should_compact,
    DEFAULT_COMPACTION_SETTINGS,
};
use maho_agent::harness::session::types::{Entry, EntryKind, NewEntry};
use maho_agent::types::AgentMessage;
use maho_ai::types::{
    Api, AssistantMessage, ContentBlock, Message, ProviderId, StopReason, Usage, UsageCost, UserContent, UserMessage,
};

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(text)]),
        timestamp: 1,
    }))
}

fn assistant_message(text: &str, usage: Usage) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(Box::new(AssistantMessage {
        content: vec![ContentBlock::text(text)],
        api: Api::from("anthropic-messages"),
        provider: ProviderId::from("anthropic"),
        model: String::new(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage,
        stop_reason: StopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    })))
}

fn message_entry(id: &str, parent_id: Option<&str>, message: AgentMessage, seq: i64) -> Entry {
    let entry = NewEntry::message(id, parent_id.map(str::to_owned), message);
    Entry {
        id: entry.id,
        parent_id: entry.parent_id,
        seq,
        timestamp: seq,
        kind: entry.kind,
    }
}

fn custom_entry(id: &str, parent_id: Option<&str>, seq: i64) -> Entry {
    let entry = NewEntry::custom(id, parent_id.map(str::to_owned), "custom");
    Entry {
        id: entry.id,
        parent_id: entry.parent_id,
        seq,
        timestamp: seq,
        kind: entry.kind,
    }
}

#[test]
fn calculates_total_context_tokens_from_usage() {
    assert_eq!(calculate_context_tokens(&usage(1000, 500, 200, 100)), 1800);
    assert_eq!(calculate_context_tokens(&usage(0, 0, 0, 0)), 0);
}

#[test]
fn checks_compaction_threshold() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10000,
        keep_recent_tokens: 20000,
    };
    assert!(should_compact(95000, 100000, &settings));
    assert!(!should_compact(89000, 100000, &settings));
    assert!(!should_compact(
        95000,
        100000,
        &CompactionSettings {
            enabled: false,
            ..settings
        }
    ));
}

#[test]
fn default_settings_match_the_harness_defaults() {
    assert_eq!(
        DEFAULT_COMPACTION_SETTINGS,
        CompactionSettings {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
        }
    );
}

#[test]
fn finds_a_cut_point_based_on_token_differences() {
    let mut entries: Vec<Entry> = Vec::new();
    let mut parent_id: Option<String> = None;
    for index in 0..10 {
        let user = message_entry(
            &format!("u{index}"),
            parent_id.as_deref(),
            user_message(&format!("User {index}")),
            (index * 2 + 1) as i64,
        );
        let user_id = user.id.clone();
        entries.push(user);
        let assistant = message_entry(
            &format!("a{index}"),
            Some(&user_id),
            assistant_message(
                &format!("Assistant {index}"),
                usage(0, 100, ((index + 1) * 1000) as u64, 0),
            ),
            (index * 2 + 2) as i64,
        );
        parent_id = Some(assistant.id.clone());
        entries.push(assistant);
    }

    let result = find_cut_point(&entries, 0, entries.len(), 2500);
    assert_eq!(entries[result.first_kept_entry_index].entry_type(), maho_agent::harness::session::types::EntryType::Message);
}

#[test]
fn covers_cut_point_and_turn_start_edge_cases() {
    let first_custom = custom_entry("first", None, 1);
    let second_custom = custom_entry("second", Some("first"), 2);
    let second_custom_for_turn = second_custom.clone();
    let result = find_cut_point(&[first_custom.clone(), second_custom], 0, 2, 1);
    assert_eq!(result.first_kept_entry_index, 0);
    assert_eq!(result.turn_start_index, -1);
    assert!(!result.is_split_turn);

    let branch_summary = Entry {
        id: "branch".to_owned(),
        parent_id: None,
        seq: 2,
        timestamp: 2,
        kind: EntryKind::BranchSummary {
            from_id: None,
            summary: "summary".to_owned(),
            details: None,
            usage: None,
            from_hook: false,
        },
    };
    assert_eq!(find_turn_start_index(&[first_custom.clone(), branch_summary.clone()], 1, 0), 1);
    assert_eq!(find_turn_start_index(&[first_custom.clone(), second_custom_for_turn], 1, 0), -1);

    let result = find_cut_point(&[first_custom, branch_summary], 0, 2, 1);
    assert_eq!(result.first_kept_entry_index, 0);

    let tool_result = message_entry(
        "tool",
        None,
        AgentMessage::Llm(Message::ToolResult(maho_ai::types::ToolResultMessage {
            tool_call_id: "call-1".to_owned(),
            tool_name: "read".to_owned(),
            content: vec![ContentBlock::text("tool output")],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 1,
        })),
        1,
    );
    let result = find_cut_point(&[tool_result], 0, 1, 1);
    assert_eq!(result.first_kept_entry_index, 0);
    assert_eq!(result.turn_start_index, -1);
    assert!(!result.is_split_turn);

    let user = message_entry("user", None, user_message("user"), 1);
    let compaction = Entry {
        id: "compaction".to_owned(),
        parent_id: Some("user".to_owned()),
        seq: 2,
        timestamp: 2,
        kind: EntryKind::Compaction {
            summary: "summary".to_owned(),
            retained_tail: Vec::new(),
            tokens_before: 1,
            details: None,
            usage: None,
            from_hook: false,
        },
    };
    let assistant = message_entry("assistant", Some("compaction"), assistant_message("assistant", usage(0, 100, 0, 0)), 3);
    assert_eq!(
        find_cut_point(&[user, compaction, assistant], 0, 3, 1).first_kept_entry_index,
        2
    );
}

#[test]
fn estimates_tokens_for_every_message_kind() {
    let user = user_message("12345678");
    assert_eq!(estimate_tokens(&user), 2);

    let assistant = assistant_message("12345678", usage(1, 1, 0, 0));
    assert_eq!(estimate_tokens(&assistant), 2);

    let branch_summary = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::BranchSummary(
        maho_agent::harness::messages::create_branch_summary_message("12345678", None, 1),
    ));
    let compaction_summary = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::CompactionSummary(
        maho_agent::harness::messages::create_compaction_summary_message("12345678", 1, 1),
    ));
    assert!(estimate_tokens(&branch_summary) > 0);
    assert!(estimate_tokens(&compaction_summary) > 0);
}

#[test]
fn returns_usage_from_the_last_valid_assistant_message() {
    let entries = vec![
        message_entry("u1", None, user_message("one"), 1),
        message_entry("a1", Some("u1"), assistant_message("one", usage(1, 2, 3, 4)), 2),
    ];
    assert_eq!(get_last_assistant_usage(&entries), Some(usage(1, 2, 3, 4)));

    let mut aborted = assistant_message("two", usage(9, 9, 9, 9));
    if let AgentMessage::Llm(Message::Assistant(assistant)) = &mut aborted {
        assistant.stop_reason = StopReason::Aborted;
    }
    let entries = vec![message_entry("a2", None, aborted, 3)];
    assert_eq!(get_last_assistant_usage(&entries), None);
}

#[test]
fn estimates_context_tokens_from_usage_plus_trailing_messages() {
    let messages = vec![
        user_message("12345678"),
        assistant_message("12345678", usage(100, 50, 0, 0)),
        user_message("1234"),
    ];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 150);
    assert_eq!(estimate.trailing_tokens, 1);
    assert_eq!(estimate.tokens, 151);
    assert_eq!(estimate.last_usage_index, Some(1));

    let without_usage = estimate_context_tokens(&[user_message("12345678")]);
    assert_eq!(without_usage.usage_tokens, 0);
    assert_eq!(without_usage.last_usage_index, None);
    assert_eq!(without_usage.tokens, 2);
}

#[test]
fn serializes_a_conversation_for_summarization() {
    let messages = vec![
        Message::User(UserMessage {
            content: UserContent::Blocks(vec![ContentBlock::text("hello")]),
            timestamp: 1,
        }),
        Message::Assistant(Box::new(match assistant_message("world", usage(1, 1, 0, 0)) {
            AgentMessage::Llm(Message::Assistant(assistant)) => *assistant,
            _ => unreachable!(),
        })),
    ];
    let serialized = serialize_conversation(&messages);
    assert_eq!(serialized, "[User]: hello\n\n[Assistant]: world");
}

#[test]
fn prepare_compaction_returns_none_for_empty_paths_and_trailing_compactions() {
    assert!(prepare_compaction(&[], DEFAULT_COMPACTION_SETTINGS).expect("prepare").is_none());

    let entries = vec![Entry {
        id: "c1".to_owned(),
        parent_id: None,
        seq: 1,
        timestamp: 1,
        kind: EntryKind::Compaction {
            summary: "summary".to_owned(),
            retained_tail: Vec::new(),
            tokens_before: 10,
            details: None,
            usage: None,
            from_hook: false,
        },
    }];
    assert!(prepare_compaction(&entries, DEFAULT_COMPACTION_SETTINGS).expect("prepare").is_none());
}

#[test]
fn prepare_compaction_splits_history_and_retained_tail() {
    let mut entries: Vec<Entry> = Vec::new();
    let mut parent_id: Option<String> = None;
    for index in 0..8 {
        let user = message_entry(
            &format!("u{index}"),
            parent_id.as_deref(),
            user_message(&format!("User {index} {}", "x".repeat(400))),
            (index * 2 + 1) as i64,
        );
        let user_id = user.id.clone();
        entries.push(user);
        let assistant = message_entry(
            &format!("a{index}"),
            Some(&user_id),
            assistant_message(&format!("Assistant {index}"), usage(0, 100, 1000, 0)),
            (index * 2 + 2) as i64,
        );
        parent_id = Some(assistant.id.clone());
        entries.push(assistant);
    }

    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 1000,
        keep_recent_tokens: 500,
    };
    let preparation = prepare_compaction(&entries, settings)
        .expect("prepare")
        .expect("applicable");
    assert!(!preparation.retained_tail.is_empty());
    assert!(preparation.tokens_before > 0);
    assert_eq!(preparation.settings.keep_recent_tokens, 500);
    assert!(!preparation.messages_to_summarize.is_empty());
}
