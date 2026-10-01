//! Translates `tools/task/renderers.test.ts`, `tools/task/renderer-text.test.ts`, `tools/task/renderers-run-stats.test.ts`, and `tools/task/renderers-invalid-arguments.test.ts`.

use pretty_assertions::assert_eq;

use crate::state::{ResolvedModelRecord, ResolvedModelSource, TaskRunStats};
use crate::tools::render::test_theme::AnsiTheme;
use crate::tools::render::{Lines, LinesComponent, RendererTheme, ThemeColor};
use crate::tools::task::call_renderer::TaskCallArgs;
use crate::tools::task::renderers::*;
use crate::tools::task::types::{TaskToolDetails, TaskToolItemDetail, TaskToolMode};

/// `TEST_THEME` from renderers-run-stats.test.ts: identity styling.
struct PlainTheme;

impl RendererTheme for PlainTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        text.to_string()
    }
    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
}

/// `RECORDING_THEME` from renderers-invalid-arguments.test.ts.
struct RecordingTheme;

impl RendererTheme for RecordingTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        format!("<{}>{text}</{}>", color.as_str(), color.as_str())
    }
    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
}

const OBSERVED_REASON: &str = "Provide EITHER prompt OR tasks, not both";

/// `TERMINAL_CONTROL_PATTERN = /[\u0000-\u001f\u007f-\u009f]/u`, hand-written.
fn expect_no_terminal_controls(value: &str) {
    let has_control = value
        .chars()
        .any(|ch| ('\u{0}'..='\u{1f}').contains(&ch) || ('\u{7f}'..='\u{9f}').contains(&ch));
    assert!(!has_control, "{value:?}");
}

fn model_record(
    provider: &str,
    model_id: &str,
    display: &str,
    reasoning_effort: Option<&str>,
    variant: Option<&str>,
) -> ResolvedModelRecord {
    let mut record = ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id);
    record.display = display.to_string();
    record.reasoning_effort = reasoning_effort.map(str::to_string);
    record.variant = variant.map(str::to_string);
    record
}

// NOTE: the given Rust API does not show TaskRunStats' full field list or derives. This assumes it
// derives Default and only sets the fields the renderer reads (runtime_ms, tool_calls, cost_usd,
// cache_hit_rate_run, tokens_per_second); TS-only facts like turns/output_tokens/total_tokens/
// generation_ms/cache_hit_rate_last do not affect the rendered row.
fn run_stats(
    runtime_ms: u64,
    tool_calls: u32,
    tokens_per_second: Option<f64>,
    cost_usd: Option<f64>,
    cache_hit_rate_run: Option<f64>,
) -> TaskRunStats {
    TaskRunStats {
        runtime_ms,
        tool_calls: tool_calls.into(),
        tokens_per_second,
        cost_usd,
        cache_hit_rate_run,
        ..Default::default()
    }
}

fn first_line(lines: Vec<String>) -> String {
    lines.into_iter().next().unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// renderers.test.ts: statusThemeColor
// ---------------------------------------------------------------------------------------------

#[test]
fn terminal_statuses_map_to_success_error_warning_colors() {
    // then
    assert_eq!(status_theme_color("completed"), ThemeColor::Success);
    assert_eq!(status_theme_color("error"), ThemeColor::Error);
    assert_eq!(status_theme_color("cancelled"), ThemeColor::Warning);
    assert_eq!(status_theme_color("running"), ThemeColor::Accent);
    assert_eq!(status_theme_color("lost"), ThemeColor::Error);
}

// ---------------------------------------------------------------------------------------------
// renderers.test.ts: taskCallLines
// ---------------------------------------------------------------------------------------------

#[test]
fn spawn_call_plain_row_includes_task_prompt_and_mode() {
    // given
    let args = TaskCallArgs {
        prompt: Some("ship it".to_string()),
        subagent_type: Some("atlas".to_string()),
        run_in_background: Some(false),
        ..Default::default()
    };

    // when
    let lines = task_call_lines(&args);

    // then
    assert_eq!(lines, vec![r#"task "ship it" foreground"#.to_string()]);
}

#[test]
fn category_spawn_call_prompt_and_mode_stay_stable() {
    // given
    let args = TaskCallArgs {
        prompt: Some("inspect task rendering".to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(false),
        ..Default::default()
    };

    // when
    let lines = task_call_lines(&args);

    // then
    assert_eq!(lines, vec![r#"task "inspect task rendering" foreground"#.to_string()]);
}

#[test]
fn spawn_call_target_and_mode_are_summarized() {
    // when
    let lines = task_call_lines(&TaskCallArgs {
        prompt: Some("x".to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        ..Default::default()
    });

    // then
    let joined = lines.join(" ");
    assert!(!joined.contains("quick"), "{joined}");
    assert!(joined.contains("background"), "{joined}");
}

#[test]
fn spawn_call_without_target_falls_back_to_generic_task_label() {
    // when
    let lines = task_call_lines(&TaskCallArgs { prompt: Some("more".to_string()), ..Default::default() });

    // then
    let joined = lines.join(" ");
    assert!(joined.contains("task"), "{joined}");
}

#[test]
fn long_multiline_korean_english_prompt_is_normalized_and_width_safe() {
    // given
    let prompt = [
        "실제 프롬프트 첫 줄입니다.",
        "Second line is deliberately long enough to require a concise terminal excerpt.",
    ]
    .join("\n");

    // when
    let line = first_line(task_call_lines(&TaskCallArgs {
        prompt: Some(prompt),
        category: Some("ultrabrain".to_string()),
        run_in_background: Some(true),
        ..Default::default()
    }));

    // then
    assert!(line.contains("실제 프롬프트"), "{line}");
    assert!(line.contains("Second line"), "{line}");
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains("..."), "{line}");
    assert!(renderer_visible_width(&line) <= 100, "{line}");
}

#[test]
fn task_summary_replaces_truncated_prompt_excerpt() {
    // given
    let args = TaskCallArgs {
        prompt: Some(
            "TASK: A very long internal delegation prompt that the user should not have to read in the call row."
                .to_string(),
        ),
        task_summary: Some("Audit the task tool boundary".to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(false),
        ..Default::default()
    };

    // when
    let lines = task_call_lines(&args);

    // then
    assert_eq!(lines, vec![r#"task "Audit the task tool boundary" foreground"#.to_string()]);
}

#[test]
fn task_summary_replaces_prompt_excerpt_when_width_bounded() {
    // given
    let args = TaskCallArgs {
        prompt: Some(
            "TASK: A very long internal delegation prompt that the user should not have to read in the call row."
                .to_string(),
        ),
        task_summary: Some("Audit the task tool boundary".to_string()),
        run_in_background: Some(true),
        ..Default::default()
    };

    // when
    let line = first_line(render_task_call_lines(&args, &AnsiTheme, Some(100)));

    // then
    assert!(line.contains("Audit the task tool boundary"), "{line}");
    assert!(!line.contains("internal delegation"), "{line}");
}

#[test]
fn long_korean_prompt_truncation_backs_up_to_word_boundary() {
    // given
    let sentence = "한국어로 긴 작업 지시를 작성하고 여러 줄의 혼합 폭 텍스트를 확인하세요.";
    let prompt = format!("{sentence} {sentence}");

    // when
    let line = first_line(task_call_lines(&TaskCallArgs {
        prompt: Some(prompt),
        category: Some("missing-cat".to_string()),
        run_in_background: Some(false),
        ..Default::default()
    }));

    // then
    assert!(line.contains("\"한국어로 긴 작업 지시를"), "{line}");
    assert!(line.contains("..."), "{line}");
    assert!(!line.contains("한국..."), "{line}");
    assert!(renderer_visible_width(&line) <= 100, "{line}");
}

// ---------------------------------------------------------------------------------------------
// renderers.test.ts: taskResultLines
// ---------------------------------------------------------------------------------------------

#[test]
fn w2batch_aggregate_items_each_get_ordered_result_line() {
    // given
    let details = TaskToolDetails {
        task_id: "st_batch_1".to_string(),
        status: "error".to_string(),
        mode: TaskToolMode::Spawn,
        items: Some(vec![
            TaskToolItemDetail {
                task_id: "st_batch_1".to_string(),
                name: Some("alpha".to_string()),
                status: "completed".to_string(),
                ..Default::default()
            },
            TaskToolItemDetail {
                task_id: String::new(),
                name: Some("beta".to_string()),
                status: "error".to_string(),
                error_message: Some("depth limit".to_string()),
                ..Default::default()
            },
            TaskToolItemDetail {
                task_id: "st_batch_3".to_string(),
                name: Some("gamma".to_string()),
                status: "pending".to_string(),
                queue_position: Some(2),
                ..Default::default()
            },
        ]),
        ..Default::default()
    };

    // when
    let lines = task_result_lines(&details);

    // then
    assert_eq!(lines.len(), 4);
    assert!(lines[1].contains("alpha"), "{}", lines[1]);
    assert!(lines[1].contains("completed"), "{}", lines[1]);
    assert!(lines[2].contains("beta"), "{}", lines[2]);
    assert!(lines[2].contains("depth limit"), "{}", lines[2]);
    assert!(lines[3].contains("gamma"), "{}", lines[3]);
    assert!(lines[3].contains("queue:2"), "{}", lines[3]);
}

#[test]
fn result_detail_shows_task_id_and_status() {
    // when
    let lines = task_result_lines(&TaskToolDetails {
        task_id: "st_0000000b".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        ..Default::default()
    });

    // then
    let joined = lines.join(" ");
    assert!(joined.contains("st_0000000b"), "{joined}");
    assert!(joined.contains("completed"), "{joined}");
}

#[test]
fn resolved_category_metadata_shows_target_model_mode_status_id_queue() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000c".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("ultrabrain".to_string()),
        model: Some("openai/gpt-5.6-sol".to_string()),
        resolved_model: Some(model_record("openai", "gpt-5.6-sol", "GPT-5.6 Sol", Some("xhigh"), Some("xhigh"))),
        run_in_background: Some(true),
        queue_position: Some(3),
        reason: Some("provider capacity".to_string()),
        ..Default::default()
    };

    // when
    let row = task_result_lines(&details).join(" ");

    // then
    assert!(row.contains("category:ultrabrain(openai/gpt-5.6-sol:xhigh)"), "{row}");
    assert_eq!(row.matches("xhigh").count(), 1, "{row}");
    assert!(row.contains("background"), "{row}");
    assert!(row.contains("pending"), "{row}");
    assert!(row.contains("id:st_0000000c"), "{row}");
    assert!(row.contains("queue:3"), "{row}");
    assert!(row.contains("reason:provider capacity"), "{row}");
}

#[test]
fn resolved_category_metadata_with_variant_only_shows_variant() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000c".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("ultrabrain".to_string()),
        resolved_model: Some(model_record("openai", "gpt-5.6-sol", "GPT-5.6 Sol", Some("xhigh"), Some("sol"))),
        ..Default::default()
    };

    // when
    let row = task_result_lines(&details).join(" ");

    // then
    assert!(row.contains("category:ultrabrain(openai/gpt-5.6-sol:xhigh)"), "{row}");
    assert!(!row.contains(":sol"), "{row}");
}

#[test]
fn resolved_category_metadata_with_reasoning_effort_only_shows_effort() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000c".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("ultrabrain".to_string()),
        resolved_model: Some(model_record("openai", "gpt-5.6-sol", "GPT-5.6 Sol", Some("xhigh"), None)),
        ..Default::default()
    };

    // when
    let row = task_result_lines(&details).join(" ");

    // then
    assert!(row.contains("category:ultrabrain(openai/gpt-5.6-sol:xhigh)"), "{row}");
}

#[test]
fn resolved_category_metadata_without_effort_or_variant_has_no_suffix() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000c".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("ultrabrain".to_string()),
        resolved_model: Some(model_record("openai", "gpt-5.6-sol", "GPT-5.6 Sol", None, None)),
        ..Default::default()
    };

    // when
    let row = task_result_lines(&details).join(" ");

    // then
    assert!(row.contains("category:ultrabrain(openai/gpt-5.6-sol)"), "{row}");
    assert!(!row.contains(":xhigh"), "{row}");
    assert!(!row.contains(":sol"), "{row}");
}

#[test]
fn legacy_explicit_model_result_uses_raw_model_without_empty_labels() {
    // when
    let row = task_result_lines(&TaskToolDetails {
        task_id: "st_0000000d".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        subagent_type: Some("momus".to_string()),
        model: Some("openai/manual".to_string()),
        run_in_background: Some(false),
        ..Default::default()
    })
    .join(" ");

    // then
    assert!(row.contains("agent:momus(openai/manual)"), "{row}");
    assert!(row.contains("foreground"), "{row}");
    assert!(!row.contains("prompt:"), "{row}");
    assert!(!row.contains("reason:"), "{row}");
    assert!(!row.contains("[object Object]"), "{row}");
}

#[test]
fn provider_prefixed_display_is_not_duplicated_in_full_and_compact_rows() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000e".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("ultrabrain".to_string()),
        resolved_model: Some(model_record("openai", "gpt-5.6-sol", "OpenAI GPT-5.6 SOL", Some("xhigh"), None)),
        run_in_background: Some(true),
        ..Default::default()
    };

    // when
    let plain = task_result_lines(&details).join(" ");
    let compact = render_task_result_component(&details, &AnsiTheme).render(96).join(" ");

    // then
    assert!(plain.contains("category:ultrabrain(openai/gpt-5.6-sol:xhigh)"), "{plain}");
    assert!(compact.contains("category:ultrabrain(openai/gpt-5.6-sol:xhigh)"), "{compact}");
}

#[test]
fn result_component_at_width_80_keeps_model_and_reasoning_within_bounds() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000e".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("ultrabrain".to_string()),
        resolved_model: Some(model_record("openai", "gpt-5.6-sol", "GPT-5.6 Sol", Some("xhigh"), None)),
        run_in_background: Some(true),
        queue_position: Some(12),
        reason: Some("긴 대기열 사유입니다. Provider capacity is constrained for this request.".to_string()),
        ..Default::default()
    };

    // when
    let rendered = render_task_result_component(&details, &AnsiTheme).render(80);

    // then
    let joined = rendered.join(" ");
    assert!(joined.contains("category:ultrabrain(openai/gpt-5.6-sol:xhigh)"), "{joined}");
    for line in &rendered {
        assert!(renderer_visible_width(line) <= 80, "{line}");
    }
}

#[test]
fn two_fallback_attempts_keep_canonical_model_and_fallback_count_visible() {
    // given
    let details = TaskToolDetails {
        task_id: "st_0000000f".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("quick".to_string()),
        resolved_model: Some(model_record(
            "quotio-openai",
            "gpt-5.6-luna-fast",
            "gpt-5.6-luna-fast",
            Some("high"),
            None,
        )),
        fallback_attempts: Some(vec![
            model_record("kimi-coding", "kimi-for-coding-highspeed", "kimi-for-coding-highspeed", None, None),
            model_record("quotio-openai", "gpt-5.6-luna-fast", "gpt-5.6-luna-fast", Some("high"), None),
        ]),
        run_in_background: Some(false),
        ..Default::default()
    };

    // when
    let plain = task_result_lines(&details).join(" ");
    let compact = render_task_result_component(&details, &AnsiTheme).render(120).join(" ");

    // then
    for row in [&plain, &compact] {
        assert!(row.contains("category:quick(quotio-openai/gpt-5.6-luna-fast:high)"), "{row}");
        assert!(row.contains("fallback:2"), "{row}");
    }
    assert!(renderer_visible_width(&compact) <= 120, "{compact}");
}

// ---------------------------------------------------------------------------------------------
// renderers.test.ts: renderer grammar
// ---------------------------------------------------------------------------------------------

#[test]
fn injected_ansi_is_removed_while_trusted_theme_ansi_remains() {
    // given
    let call_args = TaskCallArgs {
        prompt: Some("검토 \u{1b}[31m빨강\u{1b}[0m 완료".to_string()),
        category: Some("quick\u{1b}[2J".to_string()),
        run_in_background: Some(false),
        ..Default::default()
    };
    let result_details = TaskToolDetails {
        task_id: "st_\u{1b}]8;;https://example.com\u{7}링크\u{1b}]8;;\u{7}".to_string(),
        status: "completed\u{7}".to_string(),
        mode: TaskToolMode::Spawn,
        reason: Some("정상\u{7f} 종료".to_string()),
        run_in_background: Some(true),
        ..Default::default()
    };

    // when
    let call = render_task_call_lines(&call_args, &AnsiTheme, None).join(" ");
    let result = render_task_result_lines(&result_details, &AnsiTheme).join(" ");
    let plain = task_call_lines(&call_args)
        .into_iter()
        .chain(task_result_lines(&result_details))
        .collect::<Vec<_>>()
        .join(" ");

    // then
    assert!(call.contains("\u{1b}[3mforeground\u{1b}[0m"), "{call:?}");
    assert!(result.contains("\u{1b}[3mbackground\u{1b}[0m"), "{result:?}");
    assert!(!call.contains("\u{1b}[31m"), "{call:?}");
    assert!(!call.contains("\u{1b}[2J"), "{call:?}");
    assert!(!result.contains("https://example.com"), "{result:?}");
    expect_no_terminal_controls(&plain);
}

// ---------------------------------------------------------------------------------------------
// renderer-text.test.ts
// ---------------------------------------------------------------------------------------------

#[test]
fn long_multiline_text_excerpt_at_72_is_normalized_and_bounded() {
    // given
    let text = [
        "첫 번째 줄은 아주 긴 한국어 설명입니다.",
        "Second line keeps enough English words to prove mixed-width truncation is terminal-aware.",
    ]
    .join("\n");

    // when
    let excerpt = excerpt_renderer_text(&text, Some(72));

    // then
    assert!(!excerpt.contains('\n'), "{excerpt}");
    assert!(excerpt.contains(' '), "{excerpt}");
    assert!(excerpt.contains("..."), "{excerpt}");
    expect_no_terminal_controls(&excerpt);
    assert!(renderer_visible_width(&excerpt) <= 72, "{excerpt}");
}

#[test]
fn adversarial_terminal_sequences_are_removed_without_damaging_text() {
    // given
    let cases: [(&str, &str); 9] = [
        ("앞 \u{1b}[31m빨강\u{1b}[0m 뒤", "앞 빨강 뒤"),
        ("앞 \u{1b}]8;;https://example.com\u{7}링크\u{1b}]8;;\u{7} 뒤", "앞 링크 뒤"),
        ("한\u{1b}]0;창 제목\u{1b}\\글", "한글"),
        ("한\u{7}글", "한글"),
        ("한\u{1b}[2J글", "한글"),
        ("한\u{1b}c글", "한글"),
        ("한\u{7f}\u{85}글", "한글"),
        ("안전\u{1b}]8;;https://example.com/숨김", "안전"),
        ("  첫째\t둘째\n界  ", "첫째 둘째 界"),
    ];

    // when
    let normalized: Vec<String> = cases.iter().map(|(value, _)| normalize_renderer_text(value)).collect();

    // then
    let expected: Vec<String> = cases.iter().map(|(_, expected)| expected.to_string()).collect();
    assert_eq!(normalized, expected);
    for value in &normalized {
        expect_no_terminal_controls(value);
    }
}

#[test]
fn lines_component_renders_lines_and_invalidate_is_callable() {
    // given
    let component = lines_component(Lines::Static(vec!["row one".to_string(), "row two".to_string()]));

    // when
    let rendered = component.render(80);
    component.invalidate();

    // then
    assert_eq!(rendered, vec!["row one".to_string(), "row two".to_string()]);
}

#[test]
fn long_lines_rendered_at_72_are_truncated_by_visible_width() {
    // given
    let component = lines_component(Lines::Static(vec![
        "요약: 한국어 텍스트가 길어도 셀 폭 기준으로 잘려야 합니다 and the English suffix should not overflow the terminal row."
            .to_string(),
    ]));

    // when
    let rendered = component.render(72);

    // then
    assert_eq!(rendered.len(), 1);
    assert!(rendered[0].contains("..."), "{}", rendered[0]);
    assert!(renderer_visible_width(&rendered[0]) <= 72, "{}", rendered[0]);
}

// ---------------------------------------------------------------------------------------------
// renderers-run-stats.test.ts
// ---------------------------------------------------------------------------------------------

#[test]
fn run_stats_append_runtime_and_tps_tokens() {
    // given
    let details = TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("deep".to_string()),
        execution_mode: Some("in-process".to_string()),
        model: Some("kimi-coding/kimi-k3-unlocked".to_string()),
        run_in_background: Some(false),
        run_stats: Some(run_stats(134_000, 5, Some(118.0), None, None)),
        ..Default::default()
    };

    // when
    let line = first_line(task_result_lines(&details));

    // then
    assert!(line.contains("ran:2m14s"), "{line}");
    assert!(line.contains("tools:5"), "{line}");
    assert!(line.contains("tps:118"), "{line}");
}

#[test]
fn width_aware_result_puts_run_stats_after_completed_status() {
    // given
    let details = TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        category: Some("deep".to_string()),
        execution_mode: Some("in-process".to_string()),
        model: Some("kimi-coding/kimi-k3-unlocked".to_string()),
        run_in_background: Some(false),
        run_stats: Some(run_stats(134_000, 5, Some(118.0), None, None)),
        ..Default::default()
    };

    // when
    let line = first_line(render_task_result_component(&details, &PlainTheme).render(240));

    // then
    assert!(line.contains("foreground completed"), "{line}");
    assert!(line.contains("ran:2m14s tools:5"), "{line}");
    assert!(line.contains("tps:118"), "{line}");
}

#[test]
fn run_stats_cost_then_cache_hit_then_tps_in_order() {
    // given
    let details = TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        run_in_background: Some(false),
        // NOTE: TS also sets cache_hit_rate_last: 0.2, which the renderer does not read.
        run_stats: Some(run_stats(134_000, 5, Some(118.0), Some(0.42131), Some(0.8712))),
        ..Default::default()
    };

    // when
    let line = first_line(task_result_lines(&details));

    // then: 4-decimal cost, integer percent cache hit, and tps last
    assert!(line.contains("cost:$0.4213"), "{line}");
    assert!(line.contains("ch:87%"), "{line}");
    let cost = line.find("cost:$0.4213").expect("cost token");
    let cache_hit = line.find("ch:87%").expect("cache hit token");
    let tps = line.find("tps:118").expect("tps token");
    assert!(cost < cache_hit, "{line}");
    assert!(cache_hit < tps, "{line}");
}

#[test]
fn run_stats_with_zero_cost_omit_empty_price() {
    // when
    let line = first_line(task_result_lines(&TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        run_stats: Some(run_stats(1_000, 0, None, Some(0.0), None)),
        ..Default::default()
    }));

    // then
    assert!(!line.contains("cost:"), "{line}");
    assert!(!line.contains("$0"), "{line}");
}

#[test]
fn run_stats_without_cost_or_cache_facts_show_neither_token() {
    // when
    let line = first_line(task_result_lines(&TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        run_stats: Some(run_stats(1_000, 0, Some(10.0), None, None)),
        ..Default::default()
    }));

    // then
    assert!(!line.contains("cost:"), "{line}");
    assert!(!line.contains("ch:"), "{line}");
    assert!(line.contains("tps:10"), "{line}");
}

#[test]
fn malformed_spend_facts_omit_impossible_money_and_cache_values() {
    // when
    let line = first_line(task_result_lines(&TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        // NOTE: TS also sets cache_hit_rate_last: 0.2, which the renderer does not read.
        run_stats: Some(run_stats(1_000, 0, Some(10.0), Some(f64::INFINITY), Some(2.0))),
        ..Default::default()
    }));

    // then
    assert!(!line.contains("cost:"), "{line}");
    assert!(!line.contains("ch:"), "{line}");
    assert!(line.contains("tps:10"), "{line}");
}

#[test]
fn details_without_run_stats_show_no_runtime_tokens() {
    // when
    let line = first_line(task_result_lines(&TaskToolDetails {
        task_id: "st_00000009".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        ..Default::default()
    }));

    // then
    assert!(!line.contains("ran:"), "{line}");
    assert!(!line.contains("tps:"), "{line}");
}

// ---------------------------------------------------------------------------------------------
// renderers-invalid-arguments.test.ts
// ---------------------------------------------------------------------------------------------

#[test]
fn invalid_arguments_status_uses_error_theme() {
    // when
    let color = status_theme_color("invalid_arguments");

    // then
    assert_eq!(color, ThemeColor::Error);
}

#[test]
fn malformed_task_result_is_classified_as_error_with_reason() {
    // given
    let details = TaskToolDetails {
        task_id: String::new(),
        status: "invalid_arguments".to_string(),
        mode: TaskToolMode::Spawn,
        run_in_background: Some(true),
        reason: Some(OBSERVED_REASON.to_string()),
        ..Default::default()
    };

    // when
    let rendered = render_task_result_component(&details, &RecordingTheme).render(120).join("\n");

    // then
    assert!(rendered.starts_with("<error>"), "{rendered}");
    assert!(rendered.contains("invalid_arguments"), "{rendered}");
    assert!(rendered.contains(&format!("reason:{OBSERVED_REASON}")), "{rendered}");
}