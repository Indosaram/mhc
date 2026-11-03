//! Port of the pure-helper cases of senpi packages/agent/test/harness/compaction.test.ts.

use maho_agent::harness::compaction::compaction::{
    CompactionSettings, DEFAULT_COMPACTION_SETTINGS, calculate_context_tokens,
    estimate_context_tokens, estimate_tokens, find_cut_point, find_turn_start_index,
    get_last_assistant_usage, prepare_compaction, serialize_conversation, should_compact,
};
use maho_agent::harness::session::types::{Entry, EntryKind, NewEntry};
use maho_agent::types::AgentMessage;
use maho_ai::types::{
    Api, AssistantMessage, ContentBlock, Message, ProviderId, StopReason, Usage, UsageCost,
    UserContent, UserMessage,
};

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        context_usage: None,
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
        ..Default::default()
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(text)]),
        timestamp: 1,
    }))
}

fn assistant_message(text: &str, usage: Usage) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(std::sync::Arc::new(AssistantMessage { credential_served_recently: None,
        timing: None,
        failure_source: None,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        thinking_level: None,
        provider_diagnostic: None,
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
        error_cause: None,
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

    let mut with_context = usage(1000, 500, 200, 100);
    with_context.context_usage = Some(Some(maho_ai::types::ContextUsage {
        input: 352_000,
        output: 200,
    }));
    assert_eq!(calculate_context_tokens(&with_context), 352_200);

    let mut with_null_context = usage(1000, 500, 200, 100);
    with_null_context.context_usage = Some(None);
    assert_eq!(calculate_context_tokens(&with_null_context), 0);
}

// oh-my-openagent#10792: compaction uses occupied context, not cumulative billing.
#[test]
fn keeps_cumulative_sdk_billing_below_the_context_threshold_when_the_last_request_fits() {
    let mut billed = usage(2_000, 300, 700_000, 0);
    billed.context_usage = Some(Some(maho_ai::types::ContextUsage {
        input: 352_000,
        output: 200,
    }));
    let message = assistant_message("done", billed);
    let estimate = estimate_context_tokens(&[message]);
    assert_eq!(estimate.tokens, 352_200);
    assert!(!should_compact(
        estimate.tokens,
        1_000_000,
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 300_000,
            keep_recent_tokens: 20_000,
        }
    ));
    assert_eq!(billed.total_tokens, 702_300);
}

#[test]
fn estimates_content_when_no_request_level_context_measurement_exists() {
    let mut billed = usage(700_000, 300, 0, 0);
    billed.context_usage = Some(None);
    assert_eq!(
        estimate_context_tokens(&[assistant_message("done", billed)]).tokens,
        1
    );
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
    assert_eq!(
        entries[result.first_kept_entry_index].entry_type(),
        maho_agent::harness::session::types::EntryType::Message
    );
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
    assert_eq!(
        find_turn_start_index(&[first_custom.clone(), branch_summary.clone()], 1, 0),
        1
    );
    assert_eq!(
        find_turn_start_index(&[first_custom.clone(), second_custom_for_turn], 1, 0),
        -1
    );

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
            nested_calls: None,
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
    let assistant = message_entry(
        "assistant",
        Some("compaction"),
        assistant_message("assistant", usage(0, 100, 0, 0)),
        3,
    );
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

    let branch_summary =
        AgentMessage::Custom(maho_agent::types::CustomAgentMessage::BranchSummary(
            maho_agent::harness::messages::create_branch_summary_message("12345678", None, 1),
        ));
    let compaction_summary =
        AgentMessage::Custom(maho_agent::types::CustomAgentMessage::CompactionSummary(
            maho_agent::harness::messages::create_compaction_summary_message("12345678", 1, 1),
        ));
    assert!(estimate_tokens(&branch_summary) > 0);
    assert!(estimate_tokens(&compaction_summary) > 0);
}

#[test]
fn returns_usage_from_the_last_valid_assistant_message() {
    let entries = vec![
        message_entry("u1", None, user_message("one"), 1),
        message_entry(
            "a1",
            Some("u1"),
            assistant_message("one", usage(1, 2, 3, 4)),
            2,
        ),
    ];
    assert_eq!(get_last_assistant_usage(&entries), Some(usage(1, 2, 3, 4)));

    let mut aborted = assistant_message("two", usage(9, 9, 9, 9));
    if let AgentMessage::Llm(Message::Assistant(assistant)) = &mut aborted {
        std::sync::Arc::make_mut(assistant).stop_reason = StopReason::Aborted;
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
        Message::Assistant(std::sync::Arc::new(
            match assistant_message("world", usage(1, 1, 0, 0)) {
                AgentMessage::Llm(Message::Assistant(assistant)) => std::sync::Arc::unwrap_or_clone(assistant),
                _ => unreachable!(),
            },
        )),
    ];
    let serialized = serialize_conversation(&messages);
    assert_eq!(serialized, "[User]: hello\n\n[Assistant]: world");
}

#[test]
fn prepare_compaction_returns_none_for_empty_paths_and_trailing_compactions() {
    assert!(
        prepare_compaction(&[], DEFAULT_COMPACTION_SETTINGS)
            .expect("prepare")
            .is_none()
    );

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
    assert!(
        prepare_compaction(&entries, DEFAULT_COMPACTION_SETTINGS)
            .expect("prepare")
            .is_none()
    );
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

fn agent(value: serde_json::Value) -> AgentMessage {
    serde_json::from_value(value).expect("agent message")
}

fn tool_result_message(call_id: &str, text: &str, is_error: bool) -> AgentMessage {
    agent(serde_json::json!({
        "role": "toolResult", "toolCallId": call_id, "toolName": "read",
        "content": [{ "type": "text", "text": text }], "isError": is_error, "timestamp": 1
    }))
}

fn with_assistant(message: AgentMessage, edit: impl FnOnce(&mut AssistantMessage)) -> AgentMessage {
    let mut message = message;
    if let AgentMessage::Llm(Message::Assistant(assistant)) = &mut message {
        edit(std::sync::Arc::make_mut(assistant));
    }
    message
}

#[test]
fn falls_back_to_the_last_valid_cut_point_when_the_budget_overshoots_it() {
    let big = "x".repeat(8000);
    let entries = vec![
        message_entry("u", None, user_message("u"), 1),
        message_entry(
            "a",
            Some("u"),
            assistant_message("a", usage(100, 50, 0, 0)),
            2,
        ),
        message_entry(
            "t1",
            Some("a"),
            tool_result_message("call-1", &big, false),
            3,
        ),
        message_entry(
            "t2",
            Some("t1"),
            tool_result_message("call-2", &big, false),
            4,
        ),
    ];
    let result = find_cut_point(&entries, 0, entries.len(), 100);
    assert_eq!(result.first_kept_entry_index, 1);
}

fn excludes_trailing_failed_turns_from_the_context_estimate(stop_reason: StopReason) {
    let big = "x".repeat(40_000);
    let failed = with_assistant(assistant_message(&big, usage(100, 50, 0, 0)), |assistant| {
        assistant.stop_reason = stop_reason;
        assistant.content.push(
            serde_json::from_value(serde_json::json!({
                "type": "toolCall", "id": "c-failed", "name": "bash", "arguments": { "command": "ls" }
            }))
            .expect("tool call"),
        );
    });
    let orphan = agent(serde_json::json!({
        "role": "toolResult", "toolCallId": "c-failed", "toolName": "bash",
        "content": [{ "type": "text", "text": big }], "isError": true, "timestamp": 1
    }));
    let with_failed = vec![
        user_message("Hello"),
        assistant_message("Hi", usage(100, 50, 0, 0)),
        failed,
        orphan,
        user_message("next"),
    ];
    let baseline = vec![
        user_message("Hello"),
        assistant_message("Hi", usage(100, 50, 0, 0)),
        user_message("next"),
    ];
    let with_failed = estimate_context_tokens(&with_failed);
    let baseline = estimate_context_tokens(&baseline);
    assert_eq!(with_failed.tokens, baseline.tokens);
    assert_eq!(with_failed.trailing_tokens, baseline.trailing_tokens);
}

#[test]
fn excludes_trailing_error_turns_from_the_context_estimate() {
    excludes_trailing_failed_turns_from_the_context_estimate(StopReason::Error);
}

#[test]
fn excludes_trailing_aborted_turns_from_the_context_estimate() {
    excludes_trailing_failed_turns_from_the_context_estimate(StopReason::Aborted);
}

#[test]
fn reports_last_usage_index_against_the_input_array_when_a_failed_turn_precedes_the_anchor() {
    let failed = with_assistant(
        assistant_message("failed", usage(100, 50, 0, 0)),
        |assistant| {
            assistant.stop_reason = StopReason::Error;
        },
    );
    let anchor = assistant_message("anchor", usage(200, 100, 0, 0));
    let messages = vec![
        user_message("Hello"),
        assistant_message("Hi", usage(100, 50, 0, 0)),
        failed,
        anchor.clone(),
        user_message("next"),
    ];
    let estimate = estimate_context_tokens(&messages);
    let index = estimate.last_usage_index.expect("anchor index");
    assert_eq!(messages[index], anchor);
    assert_eq!(estimate.usage_tokens, 300);
}

#[test]
fn estimates_tokens_and_context_usage_across_supported_message_roles() {
    let assistant_usage = usage(10, 5, 3, 2);
    let assistant = assistant_message("assistant", assistant_usage);
    let thinking_and_tool = with_assistant(assistant.clone(), |assistant| {
        assistant.content = serde_json::from_value(serde_json::json!([
            { "type": "thinking", "thinking": "thinking" },
            { "type": "toolCall", "id": "call-1", "name": "read", "arguments": { "path": "file.ts" } }
        ]))
        .expect("content");
    });
    let custom = agent(serde_json::json!({
        "role": "custom", "customType": "note", "content": "custom text", "display": true, "timestamp": 1
    }));
    let tool_result_with_image = agent(serde_json::json!({
        "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
        "content": [
            { "type": "text", "text": "tool text" },
            { "type": "image", "mimeType": "image/png", "data": "abc" }
        ],
        "isError": false, "timestamp": 1
    }));
    let bash = agent(serde_json::json!({
        "role": "bashExecution", "command": "npm run check", "output": "ok", "exitCode": 0,
        "cancelled": false, "truncated": false, "timestamp": 1
    }));
    let branch = agent(serde_json::json!({
        "role": "branchSummary", "summary": "branch", "fromId": "x", "timestamp": 1
    }));
    let compaction = agent(serde_json::json!({
        "role": "compactionSummary", "summary": "compact", "tokensBefore": 123, "timestamp": 1
    }));
    let plain_user =
        agent(serde_json::json!({ "role": "user", "content": "plain user", "timestamp": 1 }));

    assert!(estimate_tokens(&plain_user) > 0);
    assert!(estimate_tokens(&thinking_and_tool) > 0);
    assert!(estimate_tokens(&custom) > 0);
    assert!(estimate_tokens(&tool_result_with_image) > 1000);
    assert!(estimate_tokens(&bash) > 0);
    assert!(estimate_tokens(&branch) > 0);
    assert!(estimate_tokens(&compaction) > 0);
    // The TS `{ role: "unknown" }` case has no Rust analogue: AgentMessage is a closed enum and an
    // unknown role fails to deserialize instead of estimating to 0.
    assert!(
        serde_json::from_value::<AgentMessage>(
            serde_json::json!({ "role": "unknown", "timestamp": 1 })
        )
        .is_err()
    );

    assert_eq!(
        get_last_assistant_usage(&[
            message_entry("u1", None, user_message("user"), 1),
            message_entry("a1", Some("u1"), assistant.clone(), 2),
        ]),
        Some(assistant_usage)
    );
    let aborted = with_assistant(assistant.clone(), |a| a.stop_reason = StopReason::Aborted);
    let errored = with_assistant(assistant.clone(), |a| a.stop_reason = StopReason::Error);
    assert_eq!(
        get_last_assistant_usage(&[
            message_entry("a2", None, aborted, 3),
            message_entry("a3", Some("a2"), errored, 4),
        ]),
        None
    );
    assert_eq!(
        get_last_assistant_usage(&[
            message_entry("u2", None, user_message("user"), 5),
            message_entry("a4", Some("u2"), assistant.clone(), 6),
            message_entry(
                "a5",
                Some("a4"),
                assistant_message("partial", usage(0, 0, 0, 0)),
                7
            ),
        ]),
        Some(assistant_usage)
    );
    assert_eq!(
        estimate_context_tokens(&[user_message("no usage")]).last_usage_index,
        None
    );
    let tail = estimate_context_tokens(&[assistant.clone(), user_message("tail")]);
    assert_eq!((tail.usage_tokens, tail.last_usage_index), (20, Some(0)));
    let estimate = estimate_context_tokens(&[
        user_message("Hello"),
        assistant,
        user_message("continue"),
        assistant_message("Partial thinking", usage(0, 0, 0, 0)),
    ]);
    assert_eq!(estimate.usage_tokens, 20);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 20 + estimate.trailing_tokens);
}

fn compaction_entry(
    id: &str,
    parent_id: Option<&str>,
    summary: &str,
    retained_tail: Vec<AgentMessage>,
    details: Option<serde_json::Value>,
    seq: i64,
) -> Entry {
    Entry {
        id: id.to_owned(),
        parent_id: parent_id.map(str::to_owned),
        seq,
        timestamp: seq,
        kind: EntryKind::Compaction {
            summary: summary.to_owned(),
            retained_tail,
            tokens_before: 1234,
            details,
            usage: None,
            from_hook: false,
        },
    }
}

fn entry_message(entry: &Entry) -> AgentMessage {
    match &entry.kind {
        EntryKind::Message { message, .. } => message.clone(),
        _ => panic!("expected a message entry"),
    }
}

#[tokio::test]
async fn builds_session_context_with_a_compaction_entry() {
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_agent::harness::session::context::build_session_context;
    let u1 = message_entry("u1", None, user_message("1"), 1);
    let a1 = message_entry(
        "a1",
        Some("u1"),
        assistant_message("a", usage(100, 50, 0, 0)),
        2,
    );
    let u2 = message_entry("u2", Some("a1"), user_message("2"), 3);
    let a2 = message_entry(
        "a2",
        Some("u2"),
        assistant_message("b", usage(100, 50, 0, 0)),
        4,
    );
    let compaction = compaction_entry(
        "c1",
        Some("a2"),
        "Summary of 1,a,2,b",
        vec![
            user_message("2"),
            assistant_message("b", usage(100, 50, 0, 0)),
        ],
        None,
        5,
    );
    let u3 = message_entry("u3", Some("c1"), user_message("3"), 6);
    let a3 = message_entry(
        "a3",
        Some("u3"),
        assistant_message("c", usage(100, 50, 0, 0)),
        7,
    );
    let loaded = build_session_context(
        &[u1, a1, u2, a2, compaction, u3, a3],
        None,
        &BACKGROUND_CONTEXT,
    )
    .await;
    assert_eq!(
        loaded.iter().map(AgentMessage::role).collect::<Vec<_>>(),
        vec![
            "compactionSummary",
            "user",
            "assistant",
            "user",
            "assistant"
        ]
    );
}

#[tokio::test]
async fn prepares_compaction_using_the_latest_compaction_summary_as_previous_summary() {
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_agent::harness::session::context::build_session_context;
    let path_entries = vec![
        message_entry("u1", None, user_message("user msg 1"), 1),
        message_entry(
            "a1",
            Some("u1"),
            assistant_message("assistant msg 1", usage(100, 50, 0, 0)),
            2,
        ),
        message_entry("u2", Some("a1"), user_message("user msg 2"), 3),
        message_entry(
            "a2",
            Some("u2"),
            assistant_message("assistant msg 2", usage(5000, 1000, 0, 0)),
            4,
        ),
        compaction_entry("c1", Some("a2"), "First summary", Vec::new(), None, 5),
        message_entry("u3", Some("c1"), user_message("user msg 3"), 6),
        message_entry(
            "a3",
            Some("u3"),
            assistant_message("assistant msg 3", usage(8000, 2000, 0, 0)),
            7,
        ),
    ];
    let preparation = prepare_compaction(&path_entries, DEFAULT_COMPACTION_SETTINGS)
        .expect("prepare")
        .expect("applicable");
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("First summary")
    );
    assert!(!preparation.retained_tail.is_empty());
    let context = build_session_context(&path_entries, None, &BACKGROUND_CONTEXT).await;
    assert_eq!(
        preparation.tokens_before,
        i64::try_from(estimate_context_tokens(&context).tokens).expect("tokens")
    );
}

#[test]
fn carries_a_previous_compaction_s_retained_tail_into_the_next_preparation() {
    let retained_user = user_message("retained user");
    let retained_assistant = assistant_message("retained assistant", usage(100, 50, 0, 0));
    let compaction = compaction_entry(
        "c1",
        None,
        "previous summary",
        vec![retained_user.clone(), retained_assistant.clone()],
        None,
        1,
    );
    let user = message_entry("u1", Some("c1"), user_message("new user"), 2);
    let assistant = message_entry(
        "a1",
        Some("u1"),
        assistant_message("new assistant", usage(100, 50, 0, 0)),
        3,
    );
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 1,
    };
    let preparation = prepare_compaction(&[compaction, user.clone(), assistant.clone()], settings)
        .expect("prepare")
        .expect("applicable");
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("previous summary")
    );
    let mut all = preparation.messages_to_summarize.clone();
    all.extend(preparation.turn_prefix_messages.clone());
    all.extend(preparation.retained_tail.clone());
    assert_eq!(
        all,
        vec![
            retained_user,
            retained_assistant,
            entry_message(&user),
            entry_message(&assistant)
        ]
    );
}

#[test]
fn prepares_split_turn_compaction_with_prior_file_operation_details() {
    let u1 = message_entry("u1", None, user_message("user msg 1"), 1);
    let a1 = message_entry(
        "a1",
        Some("u1"),
        with_assistant(
            assistant_message("assistant msg 1", usage(100, 50, 0, 0)),
            |assistant| {
                assistant.content = serde_json::from_value(serde_json::json!([
                { "type": "toolCall", "id": "tool-1", "name": "write", "arguments": { "path": "written.ts" } }
            ]))
            .expect("content");
            },
        ),
        2,
    );
    let compaction = compaction_entry(
        "c1",
        Some("a1"),
        "First summary",
        Vec::new(),
        Some(
            serde_json::json!({ "readFiles": ["old-read.ts"], "modifiedFiles": ["old-edit.ts", "written.ts"] }),
        ),
        3,
    );
    let u2 = message_entry("u2", Some("c1"), user_message("large turn"), 4);
    let a2 = message_entry(
        "a2",
        Some("u2"),
        assistant_message("large assistant message", usage(100, 50, 0, 0)),
        5,
    );
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 1,
    };
    let preparation = prepare_compaction(&[u1, a1, compaction, u2, a2], settings)
        .expect("prepare")
        .expect("applicable");
    assert_eq!(
        preparation.previous_summary.as_deref(),
        Some("First summary")
    );
    assert!(preparation.is_split_turn);
    assert_eq!(
        preparation
            .turn_prefix_messages
            .iter()
            .map(AgentMessage::role)
            .collect::<Vec<_>>(),
        vec!["user"]
    );
    assert!(preparation.file_ops.read.contains("old-read.ts"));
    assert!(preparation.file_ops.edited.contains("old-edit.ts"));
    assert!(preparation.file_ops.edited.contains("written.ts"));
}

#[test]
fn serializes_conversation_with_truncated_tool_results() {
    let messages = maho_agent::harness::messages::convert_to_llm(vec![tool_result_message(
        "tc1",
        &"x".repeat(5000),
        false,
    )]);
    let result = serialize_conversation(&messages);
    assert!(result.contains("[Tool result]:"));
    assert!(result.contains("[... 3000 more characters truncated]"));
}

mod summaries {
    use super::*;
    use maho_agent::harness::compaction::compaction::{
        CompactGenerationOptions, CompactionErrorCode, CompactionPreparation,
        SummaryGenerationOptions, SummaryRequestFn, compact, compact_with_request,
        generate_summary, generate_summary_with_request,
    };
    use maho_agent::harness::compaction::utils::FileOperations;
    use maho_agent::harness::context::BACKGROUND_CONTEXT;
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, FauxModelDefinition, FauxProviderHandle,
        RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
    };
    use maho_ai::types::{BoxFuture, SimpleStreamOptions, ThinkingLevel};
    use std::sync::{Arc, Mutex};

    struct FauxAuth;
    impl maho_ai::models::ModelsAuth for FauxAuth {
        fn resolve<'a>(
            &'a self,
            _: &'a dyn maho_ai::models::Provider,
            _: &'a maho_ai::models::AuthResolutionOverrides,
        ) -> BoxFuture<
            'a,
            Result<Option<maho_ai::models::AuthResolution>, maho_ai::models::ModelsError>,
        > {
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

    /// `createFauxModel`: one faux provider per call, registered on a fresh model collection.
    fn faux_models(
        reasoning: bool,
        max_tokens: u64,
    ) -> (
        maho_ai::models::Models,
        maho_ai::model::Model,
        FauxProviderHandle,
    ) {
        static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let faux = faux_provider(RegisterFauxProviderOptions {
            provider: Some(format!("faux-compaction-{id}")),
            models: Some(vec![FauxModelDefinition {
                id: if reasoning {
                    "reasoning-model"
                } else {
                    "non-reasoning-model"
                }
                .into(),
                name: None,
                reasoning: Some(reasoning),
                input: None,
                input_limits: None,
                cost: None,
                context_window: Some(200_000),
                max_tokens: Some(max_tokens),
            }]),
            ..Default::default()
        })
        .expect("faux provider");
        let model = faux.get_model(None).expect("faux model");
        let models = maho_ai::models::create_models(Some(maho_ai::models::CreateModelsOptions {
            auth: Some(Arc::new(FauxAuth)),
            ..Default::default()
        }));
        models.set_provider(faux.provider.clone());
        (models, model, faux)
    }

    fn reply(text: &str) -> AssistantMessage {
        faux_assistant_message(text, FauxAssistantMessageOptions::default())
    }

    fn failed(stop_reason: StopReason, error_message: &str) -> AssistantMessage {
        faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(stop_reason),
                error_message: Some(error_message.into()),
                ..Default::default()
            },
        )
    }

    type Seen = Arc<Mutex<Vec<(maho_ai::types::Context, SimpleStreamOptions)>>>;

    /// `createModelsWithSimpleResponses` plus option capture: replies in order, records each request.
    type ScriptedFn = Box<
        dyn Fn(
                &maho_ai::types::Context,
                SimpleStreamOptions,
                &maho_agent::harness::context::Context,
            ) -> BoxFuture<'static, AssistantMessage>
            + Send
            + Sync,
    >;

    fn scripted(responses: Vec<AssistantMessage>) -> (SummaryRequestFn<ScriptedFn>, Seen) {
        let remaining = Arc::new(Mutex::new(std::collections::VecDeque::from(responses)));
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let request: SummaryRequestFn<ScriptedFn> = SummaryRequestFn(Box::new(
            move |ai_context: &maho_ai::types::Context,
                  options: SimpleStreamOptions,
                  _: &maho_agent::harness::context::Context| {
                record
                    .lock()
                    .expect("seen")
                    .push((ai_context.clone(), options));
                let next = remaining
                    .lock()
                    .expect("responses")
                    .pop_front()
                    .expect("No faux completeSimple response queued");
                Box::pin(async move { next }) as BoxFuture<'static, AssistantMessage>
            },
        ));
        (request, seen)
    }

    fn summary_options(
        model: &maho_ai::model::Model,
        thinking: Option<ThinkingLevel>,
    ) -> SummaryGenerationOptions {
        SummaryGenerationOptions {
            model: model.clone(),
            reserve_tokens: 2000,
            custom_instructions: None,
            previous_summary: None,
            thinking_level: thinking,
        }
    }

    fn preparation(
        to_summarize: Vec<AgentMessage>,
        turn_prefix: Vec<AgentMessage>,
        is_split_turn: bool,
        reserve_tokens: u64,
    ) -> CompactionPreparation {
        CompactionPreparation {
            messages_to_summarize: to_summarize,
            turn_prefix_messages: turn_prefix,
            retained_tail: vec![user_message("Summarize this.")],
            is_split_turn,
            tokens_before: 100,
            previous_summary: None,
            file_ops: FileOperations::default(),
            settings: CompactionSettings {
                enabled: true,
                reserve_tokens,
                keep_recent_tokens: 20,
            },
        }
    }

    fn compact_options(
        model: &maho_ai::model::Model,
        thinking: Option<ThinkingLevel>,
    ) -> CompactGenerationOptions {
        CompactGenerationOptions {
            model: model.clone(),
            custom_instructions: None,
            thinking_level: thinking,
        }
    }

    #[tokio::test]
    async fn passes_reasoning_through_generate_summary_only_for_reasoning_models_with_thinking_enabled()
     {
        let messages = vec![user_message("Summarize this.")];
        // TS "off" is the absent ThinkingLevel in Rust (the enum has no Off arm).
        for (reasoning, thinking, expected) in [
            (
                true,
                Some(ThinkingLevel::Medium),
                Some(ThinkingLevel::Medium),
            ),
            (true, None, None),
            (false, Some(ThinkingLevel::Medium), None),
        ] {
            let (_, model, _faux) = faux_models(reasoning, 8192);
            let (request, seen) = scripted(vec![reply("## Goal\nTest summary")]);
            generate_summary_with_request(
                &messages,
                &summary_options(&model, thinking),
                &request,
                &BACKGROUND_CONTEXT,
            )
            .await
            .expect("summary");
            assert_eq!(seen.lock().expect("seen")[0].1.reasoning, expected);
        }
    }

    #[tokio::test]
    async fn includes_previous_summaries_and_custom_instructions_in_generate_summary_prompts() {
        let (_, model, _faux) = faux_models(false, 8192);
        let mut usage_reply = reply("## Goal\nTest summary");
        usage_reply.usage = usage(11, 7, 0, 0);
        let (request, seen) = scripted(vec![usage_reply]);
        let options = SummaryGenerationOptions {
            custom_instructions: Some("focus".into()),
            previous_summary: Some("old summary".into()),
            ..summary_options(&model, None)
        };
        let (text, summary_usage) = generate_summary_with_request(
            &[user_message("Summarize this.")],
            &options,
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("summary");
        assert!(text.contains("Test summary"));
        assert!(summary_usage.input > 0 && summary_usage.output > 0);
        assert_eq!(
            summary_usage.total_tokens,
            summary_usage.input
                + summary_usage.output
                + summary_usage.cache_read
                + summary_usage.cache_write
        );
        let seen = seen.lock().expect("seen");
        let prompt = seen[0]
            .0
            .messages
            .iter()
            .find_map(|message| match message {
                Message::User(user) => match &user.content {
                    UserContent::Blocks(blocks) => match blocks.first() {
                        Some(ContentBlock::Text(text)) => Some(text.text.clone()),
                        _ => None,
                    },
                    UserContent::Text(text) => Some(text.clone()),
                },
                _ => None,
            })
            .expect("user prompt");
        assert!(prompt.contains("<previous-summary>\nold summary\n</previous-summary>"));
        assert!(prompt.contains("Additional focus: focus"));
    }

    #[tokio::test]
    async fn preserves_the_string_result_from_generate_summary() {
        let (models, model, faux) = faux_models(false, 8192);
        faux.set_responses(vec![reply("## Goal\nTest summary").into()]);
        let summary = generate_summary(
            &[user_message("Summarize this.")],
            &models,
            &model,
            2000,
            None,
            None,
            None,
            None,
            None,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("summary");
        assert_eq!(summary, "## Goal\nTest summary");
    }

    #[tokio::test]
    async fn returns_error_results_for_failed_or_aborted_summary_generations() {
        let (_, model, _faux) = faux_models(false, 8192);
        let messages = vec![user_message("Summarize this.")];
        let (request, _) = scripted(vec![failed(StopReason::Error, "boom")]);
        let error = generate_summary_with_request(
            &messages,
            &summary_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("failed");
        assert_eq!(error.code, CompactionErrorCode::SummarizationFailed);
        assert_eq!(error.message, "Summarization failed: boom");
        let (request, _) = scripted(vec![failed(StopReason::Aborted, "stopped")]);
        let error = generate_summary_with_request(
            &messages,
            &summary_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("aborted");
        assert_eq!(error.code, CompactionErrorCode::Aborted);
        assert_eq!(error.message, "stopped");
    }

    async fn fails_instead_of_replacing_history_with(response: AssistantMessage) {
        let (_, model, _faux) = faux_models(false, 8192);
        let (request, _) = scripted(vec![response]);
        let error = generate_summary_with_request(
            &[user_message("Summarize this.")],
            &summary_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("rejected summary");
        assert_eq!(error.code, CompactionErrorCode::SummarizationFailed);
    }

    #[tokio::test]
    async fn fails_instead_of_replacing_history_with_a_truncated_answer() {
        let mut response = reply("## Goal\nHalf a summ");
        response.stop_reason = StopReason::Length;
        fails_instead_of_replacing_history_with(response).await;
    }

    #[tokio::test]
    async fn fails_instead_of_replacing_history_with_a_tool_call() {
        let mut response = reply("");
        response.content = serde_json::from_value(serde_json::json!([
            { "type": "toolCall", "id": "call-1", "name": "read", "arguments": { "path": "a.ts" } }
        ]))
        .expect("content");
        response.stop_reason = StopReason::ToolUse;
        fails_instead_of_replacing_history_with(response).await;
    }

    #[tokio::test]
    async fn fails_instead_of_replacing_history_with_an_empty_answer() {
        fails_instead_of_replacing_history_with(reply("  \n")).await;
    }

    #[tokio::test]
    async fn fails_a_split_turn_compaction_whose_turn_prefix_summary_is_empty() {
        let (_, model, _faux) = faux_models(false, 8192);
        let (request, _) = scripted(vec![reply("## Goal\nHistory summary"), reply("")]);
        let error = compact_with_request(
            &preparation(
                vec![user_message("history")],
                vec![user_message("large turn")],
                true,
                2000,
            ),
            &compact_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("empty prefix");
        assert_eq!(error.code, CompactionErrorCode::SummarizationFailed);
    }

    #[tokio::test]
    async fn clamps_compaction_summary_max_tokens_to_the_model_output_cap() {
        let (_, model, _faux) = faux_models(false, 128_000);
        let (request, seen) = scripted(vec![
            reply("## Goal\nTest summary"),
            reply("## Goal\nTest summary"),
        ]);
        let messages = vec![user_message("Summarize this.")];
        let mut prepared = preparation(messages.clone(), messages, true, 500_000);
        prepared.tokens_before = 600_000;
        compact_with_request(
            &prepared,
            &compact_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("compact");
        let seen = seen.lock().expect("seen");
        assert_eq!(
            seen.iter()
                .map(|(_, options)| options.stream.max_tokens)
                .collect::<Vec<_>>(),
            vec![Some(128_000), Some(128_000)]
        );
        assert!(
            seen.iter()
                .all(|(_, options)| options.stream.cache_retention
                    == Some(maho_ai::types::CacheRetention::None))
        );
        assert_ne!(seen[0].1.stream.session_id, seen[1].1.stream.session_id);
    }

    #[tokio::test]
    async fn retains_per_request_retries_for_non_harness_compaction_callers() {
        let (models, model, faux) = faux_models(false, 8192);
        faux.set_responses(vec![
            failed(StopReason::Error, "rate limit exceeded").into(),
            reply("recovered summary").into(),
        ]);
        let retry = maho_ai::utils::retry::RetryPolicy {
            enabled: true,
            max_retries: 1,
            base_delay_ms: 0,
            max_agent_delay_ms: None,
            random: None,
        };
        let result = compact(
            &preparation(
                vec![user_message("Summarize this.")],
                Vec::new(),
                false,
                2000,
            ),
            &models,
            &model,
            None,
            None,
            Some(&retry),
            None,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("compact");
        assert!(result.summary.contains("recovered summary"));
    }

    #[tokio::test]
    async fn returns_compaction_error_results_without_throwing() {
        let (_, model, _faux) = faux_models(false, 8192);
        let (request, _) = scripted(vec![failed(StopReason::Error, "history failed")]);
        let error = compact_with_request(
            &preparation(
                vec![user_message("Summarize this.")],
                Vec::new(),
                false,
                2000,
            ),
            &compact_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("history failed");
        assert_eq!(error.code, CompactionErrorCode::SummarizationFailed);
        assert_eq!(error.message, "Summarization failed: history failed");
    }

    #[tokio::test]
    async fn combines_usage_for_split_turn_compaction_summaries() {
        let (_, model, _faux) = faux_models(false, 8192);
        let mut history = reply("history summary");
        history.usage = usage(1, 2, 3, 4);
        let mut prefix = reply("turn prefix summary");
        prefix.usage = usage(5, 6, 7, 8);
        let (request, _) = scripted(vec![history, prefix]);
        let messages = vec![user_message("Summarize this.")];
        let result = compact_with_request(
            &preparation(messages.clone(), messages, true, 2000),
            &compact_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("compact");
        assert_eq!(result.usage, Some(usage(6, 8, 10, 12)));
    }

    #[tokio::test]
    async fn passes_reasoning_through_turn_prefix_summaries_when_enabled() {
        let (_, model, _faux) = faux_models(true, 8192);
        let (request, seen) = scripted(vec![reply("## Original Request\nTest summary")]);
        compact_with_request(
            &preparation(
                Vec::new(),
                vec![user_message("Summarize this.")],
                true,
                2000,
            ),
            &compact_options(&model, Some(ThinkingLevel::High)),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("compact");
        assert_eq!(
            seen.lock().expect("seen")[0].1.reasoning,
            Some(ThinkingLevel::High)
        );
    }

    #[tokio::test]
    async fn returns_turn_prefix_compaction_errors_without_throwing() {
        let (_, model, _faux) = faux_models(false, 8192);
        let prepared = preparation(
            Vec::new(),
            vec![user_message("Summarize this.")],
            true,
            2000,
        );
        let (request, _) = scripted(vec![failed(StopReason::Error, "prefix failed")]);
        let error = compact_with_request(
            &prepared,
            &compact_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("prefix failed");
        assert_eq!(error.code, CompactionErrorCode::SummarizationFailed);
        assert_eq!(
            error.message,
            "Turn prefix summarization failed: prefix failed"
        );
        let (request, _) = scripted(vec![failed(StopReason::Aborted, "prefix stopped")]);
        let error = compact_with_request(
            &prepared,
            &compact_options(&model, None),
            &request,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect_err("prefix aborted");
        assert_eq!(error.code, CompactionErrorCode::Aborted);
        assert_eq!(error.message, "prefix stopped");
    }

    #[tokio::test]
    async fn returns_a_compaction_result_with_file_details() {
        let a1 = with_assistant(
            assistant_message("calling tool", usage(1000, 200, 0, 0)),
            |assistant| {
                assistant.content = serde_json::from_value(serde_json::json!([
                { "type": "toolCall", "id": "tool-1", "name": "read", "arguments": { "path": "src/index.ts" } }
            ]))
            .expect("content");
            },
        );
        let entries = vec![
            message_entry("u1", None, user_message("read a file"), 1),
            message_entry("a1", Some("u1"), a1, 2),
            message_entry("u2", Some("a1"), user_message("continue"), 3),
            message_entry(
                "a2",
                Some("u2"),
                assistant_message("done", usage(4000, 500, 0, 0)),
                4,
            ),
        ];
        let prepared = prepare_compaction(&entries, DEFAULT_COMPACTION_SETTINGS)
            .expect("prepare")
            .expect("applicable");
        let (models, model, faux) = faux_models(false, 8192);
        faux.set_responses(vec![reply("## Goal\nTest summary").into()]);
        let result = compact(
            &prepared,
            &models,
            &model,
            None,
            None,
            None,
            None,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("compact");
        assert!(!result.summary.is_empty());
        assert!(result.usage.expect("usage").total_tokens > 0);
        assert!(!result.retained_tail.is_empty());
        assert!(result.details.is_some());
    }
}

#[test]
fn estimates_a_compacted_path_from_content_until_a_newer_response_reports_usage() {
    let question = user_message("question");
    let answer = assistant_message("answer", usage(190_000, 1_000, 0, 0));
    let u1 = message_entry("u1", None, question.clone(), 1);
    let a1 = message_entry("a1", Some("u1"), answer.clone(), 2);
    let compaction = compaction_entry("c1", Some("a1"), "summary", vec![question, answer], None, 3);
    let next = message_entry("n1", Some("c1"), user_message("next prompt"), 4);
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 1,
    };
    let stale = prepare_compaction(
        &[u1.clone(), a1.clone(), compaction.clone(), next.clone()],
        settings,
    )
    .expect("prepare")
    .expect("applicable");
    assert!(stale.tokens_before < 1_000);
    assert!(!should_compact(
        stale.tokens_before as u64,
        200_000,
        &settings
    ));
    let fresh = message_entry(
        "f1",
        Some("n1"),
        assistant_message("fresh", usage(195_000, 1_000, 0, 0)),
        5,
    );
    let measured = prepare_compaction(&[u1, a1, compaction, next, fresh], settings)
        .expect("prepare")
        .expect("applicable");
    assert_eq!(measured.tokens_before, 196_000);
    assert!(should_compact(
        measured.tokens_before as u64,
        200_000,
        &settings
    ));
}
