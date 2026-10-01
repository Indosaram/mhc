//! Ports of the senpi harness test cases that cover the modules ported so far.
//!
//! Sources: test/harness/convert-to-llm.test.ts, test/harness/system-prompt.test.ts,
//! test/harness/truncate.test.ts, test/harness/telemetry.test.ts, test/harness/context.test.ts,
//! test/harness/resource-formatting.test.ts (prompt-template half).

use maho_agent::harness::context::{background_context, get_telemetry_context, todo_context, with_telemetry_context};
use maho_agent::harness::messages::{TimestampInput, convert_to_llm};
use maho_agent::harness::prompt_templates::{
    format_prompt_template_invocation, parse_command_args, parse_frontmatter, substitute_args,
};
use maho_agent::harness::system_prompt::format_skills_for_system_prompt;
use maho_agent::harness::telemetry::{
    HARNESS_SPAN_NAMES, InMemoryTelemetryContext, agent_telemetry_schemas, ai_telemetry_schema,
    harness_telemetry_schema, noop_telemetry_context,
};
use maho_agent::harness::types::{PromptTemplate, Skill};
use maho_agent::harness::utils::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, format_size, truncate_head,
    truncate_tail,
};
use maho_agent::types::AgentMessage;
use maho_ai::types::{
    AssistantMessage, ContentBlock, Message, StopReason, TextContent, ToolCall, Usage, UserContent, UserMessage,
};
use serde_json::{Map, json};

fn usage() -> Usage {
    Usage {
        input: 100,
        output: 50,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 150,
        cost: Default::default(),
    }
}

fn user(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::Text(TextContent {
            text: text.to_string(),
            ..TextContent::default()
        })]),
        timestamp: 1_700_000_000_000,
    }))
}

fn failed_assistant(stop_reason: StopReason, tool_call_id: &str) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(Box::new(AssistantMessage {
        content: vec![
            ContentBlock::Text(TextContent { text: "PARTIAL_BEFORE_FAILURE".to_string(), ..TextContent::default() }),
            ContentBlock::ToolCall(ToolCall {
                id: tool_call_id.to_string(),
                name: "bash".to_string(),
                arguments: Map::from_iter([("command".to_string(), json!("ls"))]),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            }),
        ],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: String::new(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: usage(),
        stop_reason,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1_700_000_000_000,
    })))
}

fn orphan_result(tool_call_id: &str) -> AgentMessage {
    AgentMessage::Llm(Message::ToolResult(maho_ai::types::ToolResultMessage {
        tool_call_id: tool_call_id.to_string(),
        tool_name: "bash".to_string(),
        content: vec![ContentBlock::Text(TextContent {
            text: "Tool aborted".to_string(),
            ..TextContent::default()
        })],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: true,
        timestamp: 1_700_000_000_000,
    }))
}

fn kept_assistant(tool_call_id: &str) -> AgentMessage {
    let AgentMessage::Llm(Message::Assistant(mut message)) =
        failed_assistant(StopReason::Error, tool_call_id)
    else {
        unreachable!("failed_assistant always builds an assistant message");
    };
    message.stop_reason = StopReason::ToolUse;
    message.content = vec![ContentBlock::ToolCall(ToolCall {
        id: tool_call_id.to_string(),
        name: "bash".to_string(),
        arguments: Map::from_iter([("command".to_string(), json!("ls"))]),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    })];
    AgentMessage::Llm(Message::Assistant(message))
}

fn serialized(messages: &[Message]) -> String {
    serde_json::to_string(messages).expect("messages serialize")
}

// convert-to-llm.test.ts
#[test]
fn drops_an_errored_assistant_turn_and_its_orphaned_tool_result() {
    let out = convert_to_llm(vec![
        user("do X"),
        failed_assistant(StopReason::Error, "c1"),
        orphan_result("c1"),
        user("next"),
    ]);
    let serialized = serialized(&out);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|message| message.role() == "user"));
    assert!(!serialized.contains("PARTIAL_BEFORE_FAILURE"));
    assert!(!serialized.contains("c1"));
}

#[test]
fn drops_an_aborted_assistant_turn_and_its_orphaned_tool_result() {
    let out = convert_to_llm(vec![user("do X"), failed_assistant(StopReason::Aborted, "c2"), orphan_result("c2")]);
    let serialized = serialized(&out);
    assert_eq!(out.len(), 1);
    assert!(!serialized.contains("PARTIAL_BEFORE_FAILURE"));
    assert!(!serialized.contains("c2"));
}

#[test]
fn keeps_a_tool_result_whose_id_a_later_kept_assistant_re_declares() {
    let out = convert_to_llm(vec![
        user("do X"),
        failed_assistant(StopReason::Error, "c3"),
        orphan_result("c3"),
        kept_assistant("c3"),
    ]);
    let serialized = serialized(&out);
    assert!(serialized.contains("c3"));
    assert!(serialized.contains("Tool aborted"));
    assert!(!serialized.contains("PARTIAL_BEFORE_FAILURE"));
}

// system-prompt.test.ts
fn visible_skill() -> Skill {
    Skill {
        name: "visible".to_string(),
        description: "Use <this> & that".to_string(),
        content: "visible content".to_string(),
        file_path: "/skills/visible/SKILL.md".to_string(),
        disable_model_invocation: None,
    }
}

fn second_skill() -> Skill {
    Skill {
        name: "second".to_string(),
        description: "Second skill".to_string(),
        content: "second content".to_string(),
        file_path: "/skills/second/SKILL.md".to_string(),
        disable_model_invocation: None,
    }
}

fn disabled_skill() -> Skill {
    Skill {
        name: "hidden".to_string(),
        description: "Hidden".to_string(),
        content: "hidden content".to_string(),
        file_path: "/skills/hidden/SKILL.md".to_string(),
        disable_model_invocation: Some(true),
    }
}

#[test]
fn formats_visible_skills_in_order_and_skips_model_disabled_skills() {
    assert_eq!(
        format_skills_for_system_prompt(&[visible_skill(), disabled_skill(), second_skill()]),
        "The following skills provide specialized instructions for specific tasks.\nRead the full skill file when the task matches its description.\nWhen a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n\n<available_skills>\n  <skill>\n    <name>visible</name>\n    <description>Use &lt;this&gt; &amp; that</description>\n    <location>/skills/visible/SKILL.md</location>\n  </skill>\n  <skill>\n    <name>second</name>\n    <description>Second skill</description>\n    <location>/skills/second/SKILL.md</location>\n  </skill>\n</available_skills>"
    );
}

#[test]
fn returns_an_empty_string_when_no_skills_are_model_visible() {
    assert_eq!(format_skills_for_system_prompt(&[disabled_skill()]), "");
}

#[test]
fn escapes_xml_in_all_model_visible_skill_fields() {
    let output = format_skills_for_system_prompt(&[Skill {
        name: "a&b".to_string(),
        description: "Quote \"double\" and 'single'".to_string(),
        content: "content".to_string(),
        file_path: "/skills/<bad>&\"quote\"/SKILL.md".to_string(),
        disable_model_invocation: None,
    }]);
    assert!(output.contains(
        "<name>a&amp;b</name>\n    <description>Quote &quot;double&quot; and &apos;single&apos;</description>\n    <location>/skills/&lt;bad&gt;&amp;&quot;quote&quot;/SKILL.md</location>"
    ));
}

// truncate.test.ts
#[test]
fn counts_utf8_bytes_without_node_buffer() {
    let content = "aé🙂\nb";
    let result = truncate_head(content, TruncationOptions { max_bytes: Some(100), max_lines: Some(10) });
    assert!(!result.truncated);
    assert_eq!(result.total_bytes, content.len() as u64);
    assert_eq!(result.output_bytes, content.len() as u64);
    assert_eq!(result.total_bytes, 9);
}

#[test]
fn does_not_count_a_trailing_newline_as_an_extra_line() {
    let content = format!("{}\n", ["line"; 3].join("\n"));
    let head = truncate_head(&content, TruncationOptions { max_bytes: Some(100), max_lines: Some(3) });
    let tail = truncate_tail(&content, TruncationOptions { max_bytes: Some(100), max_lines: Some(3) });
    assert!(!head.truncated);
    assert_eq!((head.total_lines, head.output_lines), (3, 3));
    assert!(!tail.truncated);
    assert_eq!((tail.total_lines, tail.output_lines), (3, 3));
}

#[test]
fn truncates_head_on_utf8_byte_limits_without_partial_lines() {
    let result = truncate_head("éé\nabc", TruncationOptions { max_bytes: Some(4), max_lines: Some(10) });
    assert_eq!(result.content, "éé");
    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    assert_eq!(result.output_bytes, 4);
    assert!(!result.first_line_exceeds_limit);
}

#[test]
fn reports_head_truncation_when_the_first_line_exceeds_the_byte_limit() {
    let result = truncate_head("éé\nabc", TruncationOptions { max_bytes: Some(3), max_lines: Some(10) });
    assert_eq!(result.content, "");
    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    assert!(result.first_line_exceeds_limit);
}

#[test]
fn truncates_tail_on_utf8_boundaries_when_only_a_partial_last_line_fits() {
    let result = truncate_tail("aé🙂b", TruncationOptions { max_bytes: Some(5), max_lines: Some(10) });
    assert_eq!(result.content, "🙂b");
    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    assert!(result.last_line_partial);
    assert_eq!(result.output_bytes, 5);
}

#[test]
fn truncates_an_oversized_single_line_with_a_trailing_newline() {
    let input = format!("{}\n", "X".repeat(300_000));
    let result = truncate_tail(&input, TruncationOptions { max_bytes: Some(1024), max_lines: Some(100) });
    assert_eq!(result.content, "X".repeat(1024));
    assert_eq!(result.output_bytes, 1024);
    assert_eq!(result.output_lines, 1);
    assert!(result.last_line_partial);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
}

#[test]
fn drops_an_oversized_trailing_character_when_it_cannot_fit_in_tail_byte_limit() {
    let result = truncate_tail("abc🙂", TruncationOptions { max_bytes: Some(3), max_lines: Some(10) });
    assert_eq!(result.content, "");
    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    assert!(result.last_line_partial);
    assert_eq!(result.output_bytes, 0);
}

fn buffer_tail(content: &str, max_bytes: u64) -> String {
    let bytes = content.as_bytes();
    if bytes.len() as u64 <= max_bytes {
        return content.to_string();
    }
    let mut start = bytes.len() - max_bytes as usize;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn assert_matches_buffer_tail(input: &str, max_byte_values: Option<&[u64]>) {
    let total_bytes = input.len() as u64;
    let default_values: Vec<u64> = (0..=total_bytes + 4).collect();
    let values = max_byte_values.unwrap_or(&default_values);
    for max_bytes in values {
        let result = truncate_tail(input, TruncationOptions { max_bytes: Some(*max_bytes), max_lines: Some(10) });
        let expected = buffer_tail(input, *max_bytes);
        assert_eq!(
            result.content, expected,
            "tail mismatch input={input:?} maxBytes={max_bytes} expected={expected:?} actual={:?}",
            result.content
        );
        let output_bytes = result.content.len() as u64;
        assert!(output_bytes <= *max_bytes, "tail output exceeded byte limit input={input:?} maxBytes={max_bytes}");
    }
}

fn sampled_byte_limits(input: &str) -> Vec<u64> {
    let total_bytes = input.len() as u64;
    let candidates = [
        0,
        1,
        2,
        3,
        4,
        5,
        8,
        total_bytes / 2,
        total_bytes / 2 + 1,
        total_bytes.saturating_sub(8),
        total_bytes.saturating_sub(5),
        total_bytes.saturating_sub(4),
        total_bytes.saturating_sub(3),
        total_bytes.saturating_sub(2),
        total_bytes.saturating_sub(1),
        total_bytes,
        total_bytes + 1,
        total_bytes + 4,
    ];
    let mut values: Vec<u64> = candidates.into_iter().collect();
    values.sort_unstable();
    values.dedup();
    values
}

#[test]
fn matches_buffer_tail_truncation_semantics_across_deterministic_fuzz_cases() {
    // senpi's alphabet includes lone surrogates; a Rust `&str` cannot hold them, so the alphabet
    // is the same minus the unpaired-surrogate entries (recorded as a deviation).
    let alphabet = ["a", "\u{7f}", "\u{80}", "é", "\u{7ff}", "\u{800}", "中", "\u{d7ff}", "🙂", "\u{e000}", "\u{ffff}"];

    fn check_exhaustive(prefix: String, depth: u32) {
        assert_matches_buffer_tail(&prefix, Some(&sampled_byte_limits(&prefix)));
        if depth == 0 {
            return;
        }
        for character in ["a", "é", "中", "🙂"] {
            check_exhaustive(format!("{prefix}{character}"), depth - 1);
        }
    }
    check_exhaustive(String::new(), 3);

    let mut seed: u32 = 0x1234_5678;
    let mut random = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed as f64 / 4_294_967_296.0
    };
    for _ in 0..1_000 {
        let mut input = String::new();
        let length = (random() * 80.0) as usize;
        for _ in 0..length {
            let index = (random() * alphabet.len() as f64) as usize;
            input.push_str(alphabet[index.min(alphabet.len() - 1)]);
        }
        assert_matches_buffer_tail(&input, Some(&sampled_byte_limits(&input)));
    }
}

#[test]
fn formats_human_readable_sizes() {
    assert_eq!(format_size(0), "0B");
    assert_eq!(format_size(1023), "1023B");
    assert_eq!(format_size(1024), "1.0KB");
    assert_eq!(format_size(50 * 1024), "50.0KB");
    assert_eq!(format_size(1024 * 1024), "1.0MB");
    assert_eq!((DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES), (2000, 51_200));
}

// telemetry.test.ts
#[test]
fn serializes_both_schemas_and_lists_the_harness_spans_in_order() {
    assert!(serde_json::to_string(&ai_telemetry_schema()).is_ok());
    assert!(serde_json::to_string(&harness_telemetry_schema()).is_ok());
    assert_eq!(agent_telemetry_schemas(), vec![ai_telemetry_schema(), harness_telemetry_schema()]);
    let harness = harness_telemetry_schema();
    let keys: Vec<&String> = harness["spans"].as_object().expect("spans object").keys().collect();
    assert_eq!(
        keys,
        HARNESS_SPAN_NAMES.iter().collect::<Vec<_>>(),
        "harness span declaration order"
    );
}

#[test]
fn starts_ai_request_and_harness_spans_through_the_shared_telemetry_context() {
    let telemetry = InMemoryTelemetryContext::new();
    let context = with_telemetry_context(telemetry.context(), &background_context());
    let runtime = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
    runtime.block_on(async {
        let span_context = context.clone();
        get_telemetry_context(&context)
            .start_span(
                maho_agent::harness::telemetry::SpanOptions {
                    name: "pi.harness.step".to_string(),
                    attributes: Map::from_iter([("pi.step.kind".to_string(), json!("assistant"))]),
                },
                move |step| {
                    Box::pin(async move {
                        step.set_attributes(Map::from_iter([("pi.step.outcome".to_string(), json!("succeeded"))]));
                        step.start_span(
                            maho_agent::harness::telemetry::SpanOptions {
                                name: "pi.ai.request".to_string(),
                                attributes: Map::new(),
                            },
                            |request| {
                                Box::pin(async move {
                                    request
                                        .set_attributes(Map::from_iter([(
                                            "pi.ai.response.stop_reason".to_string(),
                                            json!("stop"),
                                        )]));
                                })
                            },
                        )
                        .await;
                        let _ = span_context.clone();
                    })
                },
            )
            .await;
    });
    let spans = telemetry.get_spans();
    assert_eq!(
        spans.iter().map(|span| span.name.as_str()).collect::<Vec<_>>(),
        vec!["pi.harness.step", "pi.ai.request"]
    );
    assert_eq!(spans[1].parent_id, Some(spans[0].id));
    assert!(spans.iter().all(|span| span.settled));
}

#[test]
fn noop_telemetry_context_runs_callbacks_without_recording() {
    let context = with_telemetry_context(noop_telemetry_context(), &background_context());
    let runtime = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
    let value = runtime.block_on(async {
        get_telemetry_context(&context)
            .start_span(
                maho_agent::harness::telemetry::SpanOptions { name: "span".to_string(), attributes: Map::new() },
                |_span| Box::pin(async { 41 + 1 }),
            )
            .await
    });
    assert_eq!(value, 42);
}

// context.test.ts
#[test]
fn uses_no_op_telemetry_when_none_is_attached() {
    let noop = noop_telemetry_context();
    let from_background = get_telemetry_context(&background_context());
    let from_todo = get_telemetry_context(&todo_context());
    assert!(std::ptr::eq(from_background.backend().as_ref(), noop.backend().as_ref()));
    assert!(std::ptr::eq(from_todo.backend().as_ref(), noop.backend().as_ref()));
}

// resource-formatting.test.ts (prompt-template half)
#[test]
fn formats_prompt_template_invocations_with_positional_arguments() {
    let template = PromptTemplate {
        name: "review".to_string(),
        description: None,
        content: "Review $1 with $ARGUMENTS".to_string(),
    };
    assert_eq!(
        format_prompt_template_invocation(&template, &["a.ts".to_string(), "care".to_string()]),
        "Review a.ts with a.ts care"
    );
}

// prompt-templates.test.ts (pure helpers)
#[test]
fn substitutes_command_arguments() {
    let content = "$1 ${@:2} $ARGUMENTS";
    assert_eq!(
        substitute_args(content, &["hello world".to_string(), "test".to_string()]),
        "hello world test hello world test"
    );
}

#[test]
fn parses_command_arguments_with_shell_style_quotes() {
    assert_eq!(parse_command_args("a \"b c\" 'd e'"), vec!["a", "b c", "d e"]);
    assert_eq!(parse_command_args("  spaced\tout  "), vec!["spaced", "out"]);
    assert!(parse_command_args("").is_empty());
}

#[test]
fn reports_a_parse_failure_for_a_malformed_frontmatter_value() {
    let error = parse_frontmatter("---\ndescription: [unterminated\n---\nBody").expect_err("malformed yaml");
    assert!(error.contains("unexpected end of the stream"), "unexpected message: {error}");
}

#[test]
fn parses_simple_frontmatter_and_body() {
    let parsed = parse_frontmatter("---\ndescription: Example\n---\nExample body").expect("parsed");
    assert_eq!(parsed.frontmatter.get("description").map(String::as_str), Some("Example"));
    assert_eq!(parsed.body, "Example body");
    let plain = parse_frontmatter("First line description\nBody").expect("parsed");
    assert!(plain.frontmatter.is_empty());
    assert_eq!(plain.body, "First line description\nBody");
}

#[test]
fn normalizes_timestamps_like_the_ts_helpers() {
    let message = maho_agent::harness::messages::create_branch_summary_message(
        "summary",
        None,
        TimestampInput::IsoString("1970-01-01T00:00:01.000Z".to_string()),
    );
    assert_eq!(message.timestamp, 1_000);
    let numeric = maho_agent::harness::messages::create_compaction_summary_message("s", 10, TimestampInput::Number(7));
    assert_eq!(numeric.timestamp, 7);
}
