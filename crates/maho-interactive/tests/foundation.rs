use maho_interactive::{
    components::footer::{FooterComponent, FooterSnapshot},
    streaming_reveal::StreamingRevealController,
    streaming_reveal_content::{BlockUnitCounter, build_display_message, visible_units},
    theme::{ColorMode, Theme},
    tool_args_reveal::ToolArgsRevealController,
    tool_result_reveal::ToolResultRevealController,
};
use serde_json::{Value, json};
fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Truecolor).expect("valid test fixture and successful operation")
}
fn snapshot() -> FooterSnapshot {
    FooterSnapshot {
        cwd: "/tmp/project".into(),
        home: Some("/home/indo".into()),
        branch: Some("main".into()),
        session_name: Some("port".into()),
        cache_read: 50.,
        cache_write: 50.,
        cost: 1.234,
        latest_cache_hit_rate: Some(25.),
        context_window: 200000.,
        context_percent: Some(12.3),
        model_id: Some("m:high".into()),
        provider: Some("test".into()),
        provider_count: 2,
        ..FooterSnapshot::default()
    }
}
#[test]
fn footer_matches_pinned_output_at_all_widths_and_themes() {
    let footer = FooterComponent::new(snapshot());
    for name in ["dark", "light", "grok-day", "grok-night"] {
        let theme = Theme::builtin(name, ColorMode::Truecolor).expect("valid test fixture and successful operation");
        for width in [40, 60, 80, 120, 200] {
            let expected = std::fs::read_to_string(format!(
                "{}/tests/golden/footer-{name}.{width}.ansi",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("valid test fixture and successful operation");
            let actual = footer.render(width, &theme).expect("valid test fixture and successful operation").join("\n");
            assert_eq!(actual, expected, "{name} at {width}");
            assert_eq!(maho_tui::utils::visible_width(&actual), width);
        }
    }
}
#[test]
fn highlight_matches_pinned_token_colors() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("golden/highlight.json")).expect("valid test fixture and successful operation");
    for case in cases {
        let actual = maho_interactive::theme::highlight_code(
            &theme(),
            case["code"].as_str().expect("valid test fixture and successful operation"),
            case["language"].as_str(),
        );
        assert_eq!(json!(actual), case["lines"], "{}", case["language"]);
    }
}
#[test]
fn working_status_matches_pinned_function_outputs() {
    use maho_interactive::working_status::*;
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("golden/working-status.json")).expect("valid test fixture and successful operation");
    for c in cases {
        let a = &c["args"];
        let actual = match c["fn"].as_str().expect("valid test fixture and successful operation") {
            "formatWorkingElapsedSeconds" => format_working_elapsed_seconds(a[0].as_f64().expect("valid test fixture and successful operation")),
            "formatWorkingStatusMessage" => format_working_status_message(
                a[0].as_str().expect("valid test fixture and successful operation"),
                a[1].as_f64().expect("valid test fixture and successful operation"),
                a[2].as_str().expect("valid test fixture and successful operation"),
            ),
            "formatToolHookStatusMessage" => format_tool_hook_status_message(
                a[0].as_str().expect("valid test fixture and successful operation"),
                a[1].as_str().expect("valid test fixture and successful operation"),
                a[2].as_f64().expect("valid test fixture and successful operation"),
            ),
            "sanitizeWorkingStatusPlainText" => {
                sanitize_working_status_plain_text(a[0].as_str().expect("valid test fixture and successful operation"))
            }
            "formatActiveToolWorkingLabel" => {
                format_active_tool_working_label(a[0].as_str().expect("valid test fixture and successful operation"), &a[1])
            }
            _ => panic!("unrecognized fixture"),
        };
        assert_eq!(json!(actual), c["result"]);
    }
}
#[test]
fn pacing_matches_pinned_function_outputs() {
    use maho_interactive::streaming_reveal_pacing::*;
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("golden/streaming-reveal-pacing.json")).expect("valid test fixture and successful operation");
    for c in cases {
        let a: Vec<f64> = c["args"]
            .as_array()
            .expect("valid test fixture and successful operation")
            .iter()
            .map(|v| v.as_f64().expect("valid test fixture and successful operation"))
            .collect();
        let actual = match c["fn"].as_str().expect("valid test fixture and successful operation") {
            "nextStep" => next_step(a[0], a[1], a[2]),
            "updateArrivalRate" => update_arrival_rate(a[0], a[1], a[2]),
            _ => panic!("unrecognized fixture"),
        };
        assert!((actual - c["result"].as_f64().expect("valid test fixture and successful operation")).abs() < 1e-10);
    }
}
#[test]
fn reveal_preserves_graphemes_and_hidden_metadata() {
    let target = json!({"role":"assistant", "content":[{"type":"thinking","thinking":"private","signature":"sig"},{"type":"text","text":"a👨‍👩‍👧‍👦éz","tag":42}]});
    let actual = build_display_message(&target, 2, true, &mut BlockUnitCounter::default());
    assert_eq!(actual["content"][0], target["content"][0]);
    assert_eq!(actual["content"][1]["text"], "a👨‍👩‍👧‍👦");
    assert_eq!(actual["content"][1]["tag"], 42);
    assert_eq!(visible_units(&target, true), 4);
}
#[test]
fn grapheme_counter_handles_combining_append_and_reset() {
    let mut counter = BlockUnitCounter::default();
    assert_eq!(counter.count(0, "a"), 1);
    assert_eq!(counter.count(0, "á"), 1);
    assert_eq!(counter.count(0, "áb"), 2);
    assert_eq!(counter.slice(0, "áb", 1), "á");
    counter.reset();
    assert_eq!(counter.count(0, "other"), 5);
}
#[test]
fn smooth_reveal_buffers_then_converges_with_explicit_ticks() {
    let target = json!({"content":[{"type":"text","text":"x".repeat(1000)}]});
    let mut controller = StreamingRevealController::new(true, 60., false);
    assert_eq!(
        controller.begin(target.clone(), 0.)["content"][0]["text"],
        ""
    );
    assert!(controller.tick(40.).is_none());
    let mut display = Value::Null;
    for tick in 5..1000 {
        if let Some(v) = controller.tick(f64::from(tick) * 20.) {
            display = v;
        }
    }
    assert_eq!(display, target);
    assert!(controller.timer_fps.is_none());
    controller.stop();
    assert!(!controller.is_pacing_head(&target));
}
#[test]
fn tool_calls_bypass_smooth_reveal() {
    let target =
        json!({"content":[{"type":"text","text":"answer"},{"type":"toolCall","id":"one"}]});
    let mut controller = StreamingRevealController::new(true, 60., false);
    assert_eq!(controller.begin(target.clone(), 0.), target);
    assert!(!controller.is_pacing_head(&target));
}
#[test]
fn tool_args_flush_exact_and_replace_component_identity() {
    let mut controller = ToolArgsRevealController::new(true, 60.);
    assert_eq!(
        controller.update("a", 1, "{\"value\":1}", 0.).1,
        Some(json!({"value":1}))
    );
    assert!(controller.update("a", 1, "{\"value\":1}", 10.).1.is_none());
    assert_eq!(
        controller.update("a", 2, "{\"value\":2}", 20.).1,
        Some(json!({"value":2}))
    );
    assert_eq!(
        controller.flush("a", json!({"exact":true}), 30.),
        Some(json!({"exact":true}))
    );
    assert!(controller.flush("a", json!({}), 40.).is_none());
}
#[test]
fn tool_result_finishes_full_content_without_losing_details() {
    let mut controller = ToolResultRevealController::new(true, 60.);
    let result = json!({"content":[{"type":"text","text":"start"},{"type":"image","data":"opaque"}],"details":{"progress":42},"isError":true});
    let first = controller.update("a", 1, result.clone(), 0.).1.expect("valid test fixture and successful operation");
    assert_eq!(first["isError"], false);
    let mut target = result;
    target["content"][0]["text"] = json!("start more text");
    controller.update("a", 1, target.clone(), 20.);
    target["isError"] = json!(false);
    assert_eq!(controller.finish("a", 30.), Some(target));
    assert!(controller.timer_fps.is_none());
}
#[test]
fn theme_cache_roundtrips_and_rejects_invalid_documents() {
    use maho_interactive::theme::{TerminalTheme, terminal_theme_cache::*};
    let dir = tempfile::tempdir().expect("valid test fixture and successful operation");
    assert_eq!(read_terminal_theme_hint(dir.path()), None);
    write_terminal_theme_hint(TerminalTheme::Light, dir.path()).expect("valid test fixture and successful operation");
    assert_eq!(
        read_terminal_theme_hint(dir.path()),
        Some(TerminalTheme::Light)
    );
    std::fs::write(
        dir.path().join("cache/terminal-theme.json"),
        "{\"terminalTheme\":\"other\"}",
    )
    .expect("valid test fixture and successful operation");
    assert_eq!(read_terminal_theme_hint(dir.path()), None);
}
#[test]
fn renderer_reference_tracks_replacement() {
    use maho_interactive::tui_renderer::*;
    use std::{cell::RefCell, rc::Rc};
    let create = |mode| {
        create_interactive_tui(
            InteractiveTuiOptions {
                tui_mode: mode,
                show_hardware_cursor: true,
                bottom_shortcut: "End".into(),
            },
            theme(),
        )
    };
    let reference = InteractiveTuiReference::new(Rc::new(RefCell::new(create(TuiMode::Regular))));
    assert!(reference.with(|t| t.base().get_show_hardware_cursor()));
    reference.replace(create(TuiMode::Fullscreen));
    assert!(reference.with(|t| matches!(t, InteractiveTui::Fullscreen(_))));
}
#[test]
fn network_errors_exclude_auth_and_require_valid_envelopes() {
    use maho_interactive::provider_error_presentation::is_network_provider_error as classify;
    assert!(classify(Some("fetch failed"), false));
    assert!(!classify(Some("auth fetch failed"), false));
    assert!(!classify(Some("fetch failed"), true));
    assert!(classify(
        Some(r#"{"error":{"type":"network","message":"connection reset"}}"#),
        true
    ));
}
#[test]
fn provider_notice_keeps_diagnostics_after_retry_completion() {
    use maho_interactive::provider_error_presentation::ProviderErrorPresentation;
    let mut p = ProviderErrorPresentation::default();
    p.retrying("fetch failed", true);
    p.retrying("fetch failed", true);
    assert_eq!(p.notices.len(), 1);
    assert_eq!(p.notices[0].details.len(), 1);
    assert!(p.notices[0].summary.is_none());
    p.finish(None, Some(3));
    assert!(p.notices[0].summary.is_some());
    p.new_turn();
    p.record("connection lost", false);
    assert_eq!(p.notices.len(), 2);
}
#[test]
fn abort_labels_preserve_provenance_and_legacy_retry_counts() {
    use maho_interactive::aborted_error_label::*;
    assert_eq!(
        aborted_error_label(Some("fetch failed"), 2, Some(AgentAbortSource::User)),
        "Operation aborted"
    );
    assert_eq!(
        aborted_error_label(Some("Aborted after 1 retry attempt"), 0, None),
        "Provider retry failed after 1 attempt"
    );
    assert_eq!(
        aborted_error_label(Some("Provider request failed: fetch failed"), 3, None),
        "Provider request failed: fetch failed"
    );
}
#[test]
fn compaction_preview_and_idle_respect_width() {
    use maho_interactive::components::status_indicator::*;
    use maho_tui::tui::Component;
    let mut status =
        StatusIndicator::compaction(CompactionStatusReason::Overflow, "Esc", theme(), 0);
    status.set_progress_text("a long streaming preview with latest trailing text", 10);
    let lines = status.render(80);
    assert_eq!(lines.len(), 1);
    assert!(maho_tui::utils::strip_terminal_sequences(&lines[0]).contains("Esc to cancel"));
    assert!(maho_tui::utils::visible_width(&lines[0]) <= 80);
    assert_eq!(IdleStatus::default().render(40), vec![" ".repeat(40); 2]);
}
