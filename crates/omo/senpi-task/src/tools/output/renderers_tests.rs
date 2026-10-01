//! `tools/output/renderers.test.ts`

use pretty_assertions::assert_eq;

use crate::state::{ResidencyState, ResolvedModelRecord, ResolvedModelSource, TaskRunStats, TaskStatus};
use crate::tools::control::tool_result::tool_result;
use crate::tools::output::output::TaskOutputInput;
use crate::tools::output::renderers::{ToolRenderResultOptions, render_task_output_call, render_task_output_result};
use crate::tools::output::types::{
    SuspendedDetails, TaskOutputDetails, TaskSnapshot, TranscriptMode, TranscriptSource,
};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};
use crate::tools::task::renderers::renderer_visible_width;

/// `fg: (color, text) => "[color]text[/color]"` with TS-style lowerCamel color names.
struct TestTheme;

fn color_name(color: ThemeColor) -> String {
    let debug = format!("{color:?}");
    let mut chars = debug.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl RendererTheme for TestTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        let name = color_name(color);
        format!("[{name}]{text}[/{name}]")
    }

    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
}

/// `fg: (_color, text) => "\u001b[33m" + text + "\u001b[0m"`.
struct AnsiTheme;

impl RendererTheme for AnsiTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        format!("\u{1b}[33m{text}\u{1b}[0m")
    }

    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
}

const RESULT_OPTIONS: ToolRenderResultOptions = ToolRenderResultOptions {
    expanded: false,
    is_partial: false,
};

fn first_line(component: &dyn LinesComponent, width: usize) -> String {
    component.render(width).into_iter().next().unwrap_or_default()
}

fn expect_no_terminal_controls(value: &str) {
    let has_control = value.chars().any(|c| {
        let code = c as u32;
        code <= 0x1f || (0x7f..=0x9f).contains(&code)
    });
    assert!(!has_control, "unexpected terminal control in {value:?}");
}

fn call_input(value: serde_json::Value) -> TaskOutputInput {
    serde_json::from_value(value).expect("valid task_output input")
}

fn snapshot() -> TaskSnapshot {
    TaskSnapshot {
        task_id: "st_done".to_string(),
        name: None,
        description: None,
        task_summary: None,
        status: TaskStatus::Completed,
        residency_state: ResidencyState::Resident,
        suspended: None,
        execution_mode: "in-process".to_string(),
        model: "raw-model".to_string(),
        resolved_model: None,
        agent_type: None,
        category: None,
        parent_session_id: "session-parent".to_string(),
        root_session_id: "session-root".to_string(),
        age_ms: 10,
        pid: None,
        child_session_id: None,
        final_response: None,
        error_message: None,
        run_stats: None,
        lost: None,
    }
}

fn running() -> TaskStatus {
    TaskStatus::parse("running").expect("running status")
}

fn render_result_line(detail: TaskOutputDetails, theme: &dyn RendererTheme, width: usize) -> String {
    let component = render_task_output_result(&tool_result("ignored", detail), &RESULT_OPTIONS, theme);
    first_line(&component, width)
}

fn status_line(snapshot: TaskSnapshot) -> String {
    render_result_line(TaskOutputDetails::Status { snapshot }, &TestTheme, 200)
}

// ---- task_output renderers ----

#[test]
fn given_task_output_arguments_when_rendering_calls_then_rows_show_target_mode_peek_and_only_relevant_tail_lines() {
    // given / when
    let tail_args = call_input(serde_json::json!({ "name": "long-running-explorer", "mode": "tail", "tail_lines": 20 }));
    let status_args = call_input(serde_json::json!({ "task_id": "st_1", "mode": "status", "tail_lines": 20 }));
    let tail_line = first_line(&render_task_output_call(&tail_args, &TestTheme), 96);
    let status_line = first_line(&render_task_output_call(&status_args, &TestTheme), 96);

    // then
    assert!(tail_line.contains("task_output"));
    assert!(tail_line.contains("target:long-running-explorer"));
    assert!(tail_line.contains("mode:tail"));
    assert!(tail_line.contains("peek"));
    assert!(tail_line.contains("tail_lines:20"));
    assert!(status_line.contains("peek"));
    assert!(!status_line.contains("tail_lines"));
}

#[test]
fn given_a_long_multiline_korean_and_english_target_when_rendering_with_ansi_at_width_72_then_target_is_normalized_truncated_and_column_safe()
 {
    // given / when
    let args = call_input(serde_json::json!({
        "name": "한국어 작업 이름이 아주 길게 이어집니다.\nEnglish task name also continues long enough to require truncation.",
        "mode": "tail",
        "tail_lines": 7,
    }));
    let line = first_line(&render_task_output_call(&args, &AnsiTheme), 72);

    // then
    assert!(!line.contains('\n'));
    assert!(line.contains("한국어 작업"));
    assert!(line.contains("..."));
    assert!(renderer_visible_width(&line) <= 72);
}

#[test]
fn given_a_width_smaller_than_the_fixed_call_tokens_when_rendering_task_output_then_the_complete_ansi_row_is_clamped() {
    // given / when
    let args = call_input(serde_json::json!({ "name": "abcdef", "mode": "status" }));
    let line = first_line(&render_task_output_call(&args, &AnsiTheme), 20);

    // then
    assert!(line.contains("..."));
    assert!(renderer_visible_width(&line) <= 20);
}

#[test]
fn given_every_result_detail_kind_when_rendering_compact_rows_then_rows_are_exhaustive_and_transcripts_are_not_echoed() {
    // given
    let details = vec![
        TaskOutputDetails::Status {
            snapshot: TaskSnapshot {
                status: running(),
                ..snapshot()
            },
        },
        TaskOutputDetails::Transcript {
            mode: TranscriptMode::Full,
            source: TranscriptSource::EventLog,
            transcript: "secret transcript body that must stay out of compact rows".to_string(),
            truncated: true,
            snapshot: snapshot(),
        },
        TaskOutputDetails::NotFound {
            reason: "No task 'missing' in this session.".to_string(),
            known_tasks: vec!["alpha".to_string()],
        },
        TaskOutputDetails::InvalidArguments {
            reason: "Provide task_id or name.".to_string(),
        },
    ];
    let count = details.len();

    // when
    let lines: Vec<String> = details
        .into_iter()
        .map(|detail| render_result_line(detail, &TestTheme, 120))
        .collect();

    // then
    assert_eq!(lines.len(), count);
    let joined = lines.join("\n");
    assert!(joined.contains("task_output st_done running"));
    assert!(joined.contains("task_output transcript st_done"));
    assert!(joined.contains("source:event-log"));
    assert!(joined.contains("truncated"));
    assert!(joined.contains("task_output not found"));
    assert!(joined.contains("known:alpha"));
    assert!(joined.contains("task_output invalid"));
    assert!(!joined.contains("secret transcript body"));
}

#[test]
fn given_a_task_summary_snapshot_when_the_status_row_renders_then_the_summary_leads_over_the_description() {
    // given / when
    let line = status_line(TaskSnapshot {
        name: Some("task-1".to_string()),
        description: Some("quick label".to_string()),
        task_summary: Some("Audit the waiting line".to_string()),
        ..snapshot()
    });

    // then
    assert!(
        line.starts_with("[success]task_output Audit the waiting line (st_done) completed"),
        "{line}"
    );
}

#[test]
fn given_a_described_task_when_the_status_row_renders_then_the_human_label_leads_and_the_id_trails() {
    // given / when
    let line = status_line(TaskSnapshot {
        name: Some("task-1".to_string()),
        description: Some("Audit the waiting line".to_string()),
        ..snapshot()
    });

    // then
    assert!(
        line.starts_with("[success]task_output Audit the waiting line (st_done) completed"),
        "{line}"
    );
}

#[test]
fn given_long_multiline_korean_and_english_known_tasks_when_rendering_not_found_at_width_96_then_known_list_is_normalized_truncated_and_column_safe()
 {
    // given
    let detail = TaskOutputDetails::NotFound {
        reason: "No task 'missing' in this session.".to_string(),
        known_tasks: vec![
            "한국어 알려진 작업 이름이 아주 길게 이어집니다.\nEnglish known task also continues long enough to require truncation."
                .to_string(),
        ],
    };

    // when
    let line = render_result_line(detail, &AnsiTheme, 96);

    // then
    assert!(!line.contains('\n'));
    assert!(line.contains("한국어 알려진"));
    assert!(line.contains("..."));
    assert!(renderer_visible_width(&line) <= 96);
}

#[test]
fn given_resolved_model_details_when_the_status_row_renders_then_the_canonical_target_token_is_used() {
    // given / when
    let with_resolved = status_line(TaskSnapshot {
        category: Some("quick".to_string()),
        resolved_model: Some(ResolvedModelRecord {
            display: "GPT-5.6 Sol".to_string(),
            reasoning_effort: Some(" ".to_string()),
            variant: Some("xhigh".to_string()),
            ..ResolvedModelRecord::new(ResolvedModelSource::Category, "openai", "gpt-5.6-sol")
        }),
        ..snapshot()
    });
    let raw = status_line(TaskSnapshot {
        model: "anthropic/claude-sonnet-4-5".to_string(),
        ..snapshot()
    });

    // then blank effort falls back to the variant, and an unresolved model keeps a model token
    assert!(with_resolved.contains("category:quick(openai/gpt-5.6-sol:xhigh)"), "{with_resolved}");
    assert!(!with_resolved.contains("reasoning"));
    assert!(raw.contains("model:anthropic/claude-sonnet-4-5"), "{raw}");
}

#[test]
fn given_injected_controls_in_task_output_model_metadata_when_the_status_row_renders_then_it_is_plain_and_sanitized() {
    // given
    let snap = TaskSnapshot {
        category: Some("quick".to_string()),
        model: "raw\u{1b}[31m-model\u{1b}[0m".to_string(),
        resolved_model: Some(ResolvedModelRecord {
            display: "GPT\u{1b}]0;hidden\u{7}-5.6 Sol".to_string(),
            reasoning_effort: Some("xhigh\u{7}".to_string()),
            variant: Some("sol\u{7f}".to_string()),
            ..ResolvedModelRecord::new(ResolvedModelSource::Category, "openai", "gpt-5.6-sol")
        }),
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("category:quick(openai/gpt-5.6-sol:xhigh)"), "{line}");
    expect_no_terminal_controls(&line);
}

#[test]
fn given_injected_controls_in_a_task_output_result_when_rendered_then_dynamic_controls_are_removed_before_trusted_theme_styling()
 {
    // given
    let details = TaskOutputDetails::InvalidArguments {
        reason: "누락 \u{1b}[31m빨강\u{1b}[0m \u{1b}]8;;https://example.com\u{1b}\\링크\u{1b}]8;;\u{1b}\\\u{7}".to_string(),
    };

    // when
    let themed = render_result_line(details.clone(), &AnsiTheme, 120);
    let plain = render_result_line(details, &TestTheme, 120);

    // then
    assert!(themed.starts_with("\u{1b}[33m"));
    assert!(!themed.contains("\u{1b}[31m"));
    assert!(!themed.contains("https://example.com"));
    expect_no_terminal_controls(&plain);
}

// ---- task_output run stats rendering ----

#[test]
fn given_a_terminal_snapshot_with_run_stats_when_the_status_row_renders_then_runtime_and_tps_are_shown() {
    // given
    let snap = TaskSnapshot {
        run_stats: Some(TaskRunStats {
            runtime_ms: 134_000u32.into(),
            turns: 3u8.into(),
            tool_calls: 5u8.into(),
            output_tokens: Some(900u16.into()),
            total_tokens: Some(4_200u16.into()),
            generation_ms: Some(7_600u16.into()),
            tokens_per_second: Some(118u8.into()),
            cost_usd: None,
            cache_hit_rate_last: None,
            cache_hit_rate_run: None,
        }),
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("task_output st_done completed"), "{line}");
    assert!(line.contains("· ran 2m 14s"), "{line}");
    assert!(line.contains("· 118 tok/s"), "{line}");
}

#[test]
fn given_run_stats_with_cost_and_cache_hits_when_the_status_row_renders_then_the_cost_token_sits_immediately_before_tps() {
    // given
    let snap = TaskSnapshot {
        run_stats: Some(TaskRunStats {
            runtime_ms: 134_000u32.into(),
            turns: 3u8.into(),
            tool_calls: 5u8.into(),
            output_tokens: Some(900u16.into()),
            total_tokens: None,
            generation_ms: None,
            tokens_per_second: Some(118u8.into()),
            cost_usd: Some(0.4213),
            cache_hit_rate_last: Some(0.2),
            cache_hit_rate_run: Some(0.8712),
        }),
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("· $0.4213 (CH: 87%) · 118 tok/s"), "{line}");
}

#[test]
fn given_run_stats_with_cost_but_no_cache_facts_when_the_status_row_renders_then_only_the_cost_is_shown_before_tps() {
    // given
    let snap = TaskSnapshot {
        run_stats: Some(TaskRunStats {
            runtime_ms: 1_000u16.into(),
            turns: 1u8.into(),
            tool_calls: 0u8.into(),
            output_tokens: None,
            total_tokens: None,
            generation_ms: None,
            tokens_per_second: Some(20u8.into()),
            cost_usd: Some(0.5),
            cache_hit_rate_last: None,
            cache_hit_rate_run: None,
        }),
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("· $0.5000 · 20 tok/s"), "{line}");
    assert!(!line.contains("CH:"));
}

#[test]
fn given_a_snapshot_without_run_stats_when_the_status_row_renders_then_no_runtime_tokens_appear() {
    // when
    let line = status_line(snapshot());

    // then
    assert!(!line.contains("ran "));
    assert!(!line.contains("tok/s"));
}

// ---- task_output suspended residency rendering ----
// Rendering keys the suspended label off `snapshot.suspended`; the residency state itself is
// not read by the row renderer.

#[test]
fn given_a_persisted_only_snapshot_when_the_status_row_renders_then_it_shows_suspended() {
    // given
    let snap = TaskSnapshot {
        status: running(),
        suspended: Some(SuspendedDetails {
            explanation: "suspended (resumes with session)".to_string(),
        }),
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("suspended"), "{line}");
    assert!(!line.contains("running"), "{line}");
}

#[test]
fn given_an_rpc_detached_snapshot_when_the_status_row_renders_then_it_shows_suspended() {
    // given
    let snap = TaskSnapshot {
        status: running(),
        suspended: Some(SuspendedDetails {
            explanation: "suspended (resumes with session)".to_string(),
        }),
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("suspended"), "{line}");
    assert!(!line.contains("running"), "{line}");
}

#[test]
fn given_a_resident_snapshot_when_the_status_row_renders_then_the_status_label_is_unchanged_regression_pin() {
    // given
    let snap = TaskSnapshot {
        status: running(),
        residency_state: ResidencyState::Resident,
        ..snapshot()
    };

    // when
    let line = status_line(snap);

    // then
    assert!(line.contains("running"), "{line}");
    assert!(!line.contains("suspended"), "{line}");
}
