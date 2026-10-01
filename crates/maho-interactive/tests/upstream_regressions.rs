use maho_interactive::{
    components::footer::{
        FooterComponent, FooterSnapshot, account_footer_suffix, format_cwd_for_footer,
    },
    theme::{
        ColorMode, TerminalTheme, Theme, ThemeBg, ThemeColor,
        theme::{
            detect_terminal_background_from_env, get_theme_export_colors, get_theme_for_rgb_color,
            parse_auto_theme_setting, resolve_theme_setting,
        },
        theme_json::{ThemeJson, validate_theme_json},
    },
    tool_progress::*,
    working_status::*,
};
use serde_json::{Value, json};
fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Truecolor).expect("valid test fixture and successful operation")
}
fn snapshot() -> FooterSnapshot {
    FooterSnapshot {
        cwd: "/tmp/project".into(),
        branch: Some("main".into()),
        context_window: 200000.,
        context_percent: Some(12.3),
        model_id: Some("test-model".into()),
        provider: Some("test".into()),
        reasoning: true,
        thinking_level: Some("high".into()),
        provider_count: 2,
        ..FooterSnapshot::default()
    }
}
fn plain(s: &FooterSnapshot, width: usize) -> String {
    maho_tui::utils::strip_terminal_sequences(
        &FooterComponent::new(s.clone())
            .render(width, &theme())
            .expect("valid test fixture and successful operation")
            .join("\n"),
    )
}
#[test]
fn cwd_sibling_is_not_inside_home() {
    assert_eq!(
        format_cwd_for_footer("/home/user2", Some("/home/user")),
        "/home/user2"
    );
}
#[test]
fn cwd_home_and_descendants_abbreviate() {
    assert_eq!(format_cwd_for_footer("/home/user", Some("/home/user")), "~");
    assert_eq!(
        format_cwd_for_footer("/home/user/project", Some("/home/user")),
        "~/project"
    );
}
#[test]
fn footer_wide_session_stays_within_budget() {
    let mut s = snapshot();
    s.session_name = Some("中文".repeat(30));
    assert!(maho_tui::utils::visible_width(&plain(&s, 93)) <= 93);
}
#[test]
fn footer_wide_model_and_provider_stay_within_budget() {
    let mut s = snapshot();
    s.model_id = Some("模".repeat(30));
    s.provider = Some("供應商".into());
    assert!(maho_tui::utils::visible_width(&plain(&s, 60)) <= 60);
}
#[test]
fn footer_model_context_and_ellipsis_survive_narrow_layout() {
    let mut s = snapshot();
    s.session_name = Some("deep-work-on-footer-layout".into());
    s.cost = 1.234;
    let p = plain(&s, 60);
    assert!(p.contains("test-model:high"));
    assert!(p.contains("main"));
    assert!(p.contains("(auto)"));
    assert!(p.contains('…'));
}
#[test]
fn footer_path_elides_before_cache_and_cost() {
    let mut s = snapshot();
    s.cwd = "/workspace/client/platform/services/senpi/packages/coding-agent".into();
    s.cost = 1.234;
    s.cache_read = 50.;
    s.cache_write = 50.;
    s.latest_cache_hit_rate = Some(25.);
    let p = plain(&s, 110);
    assert!(p.starts_with('…'));
    assert!(p.contains("coding-agent"));
    assert!(p.contains("CH25.0%"));
    assert!(p.contains("$1.234"));
}
#[test]
fn footer_model_survives_very_narrow_layout() {
    assert!(plain(&snapshot(), 30).contains("test-model"));
}
#[test]
fn footer_provider_prefix_requires_multiple_providers() {
    let mut s = snapshot();
    assert!(plain(&s, 200).contains("(test) test-model:high"));
    s.provider_count = 1;
    assert!(!plain(&s, 200).contains("(test)"));
}
#[test]
fn fast_mode_marks_model_label() {
    let mut s = snapshot();
    s.fast_mode = true;
    assert!(plain(&s, 120).contains("⚡ test-model:high"));
}
#[test]
fn nonfast_mode_has_no_lightning() {
    assert!(!plain(&snapshot(), 120).contains('⚡'));
}
#[test]
fn wide_fast_mode_includes_glyph_in_budget() {
    let mut s = snapshot();
    s.fast_mode = true;
    s.model_id = Some("模".repeat(30));
    assert!(maho_tui::utils::visible_width(&plain(&s, 60)) <= 60);
}
#[test]
fn footer_cache_threshold_retains_ten_percent() {
    let mut s = snapshot();
    s.cache_read = 100.;
    s.latest_cache_hit_rate = Some(9.9);
    assert!(!plain(&s, 160).contains("CH"));
    s.latest_cache_hit_rate = Some(10.);
    assert!(plain(&s, 160).contains("CH10.0%"));
}
#[test]
fn footer_context_not_obsolete_token_counters() {
    let mut s = snapshot();
    s.context_tokens = Some(44000.);
    s.context_window = 800000.;
    s.context_percent = Some(5.5);
    s.cache_read = 1500000.;
    s.latest_cache_hit_rate = Some(97.1);
    let p = plain(&s, 160);
    assert!(p.contains("44K/800K (5.5%) (auto)"));
    assert!(p.contains("CH97.1%"));
    assert!(!p.contains("cache "));
}
#[test]
fn kimi_costs_are_subscription_estimates() {
    let mut s = snapshot();
    s.provider = Some("kimi-coding".into());
    s.cost = 1.234;
    assert!(plain(&s, 120).contains("$1.234 (sub)"));
}
#[test]
fn sdk_marker_toggles() {
    let mut f = FooterComponent::new(snapshot());
    assert!(!f.render(120, &theme()).expect("valid test fixture and successful operation")[0].contains("SDK"));
    f.set_compaction_delegated(true);
    assert!(f.render(120, &theme()).expect("valid test fixture and successful operation")[0].contains("SDK"));
    f.set_compaction_delegated(false);
    assert!(!f.render(120, &theme()).expect("valid test fixture and successful operation")[0].contains("SDK"));
}
#[test]
fn sdk_marker_is_complete_or_absent_at_every_width() {
    let mut f = FooterComponent::new(snapshot());
    f.set_compaction_delegated(true);
    for w in 20..=100 {
        let p = maho_tui::utils::strip_terminal_sequences(&f.render(w, &theme()).expect("valid test fixture and successful operation")[0]);
        assert!(maho_tui::utils::visible_width(&p) <= w);
        if !p.contains("(SDK)") {
            assert!(!p.contains("SDK"));
            assert!(!p.contains("DK)"));
            assert!(!p.contains("K)"));
        }
    }
}
fn pooled(display: Option<&str>) -> maho_ai::auth::pool::slots::PooledCredential {
    serde_json::from_value(json!({"type":"oauth","access":"fake","refresh":"fake","expires":4102444800000f64,"pinned":"second","accounts":[{"name":"default","access":"fake"},{"name":"second","access":"fake","displayName":display}]})).expect("valid test fixture and successful operation")
}
#[test]
fn account_suffix_uses_pinned_identity() {
    assert_eq!(
        account_footer_suffix(Some(&pooled(None)), "session"),
        "@second"
    );
}
#[test]
fn account_display_label_preserves_punctuation() {
    assert_eq!(
        account_footer_suffix(Some(&pooled(Some("Work: dev (b)"))), "session"),
        "@Work: dev (b) (second)"
    );
}
#[test]
fn account_label_is_column_bounded() {
    let label = account_footer_suffix(Some(&pooled(Some(&"中".repeat(16)))), "session");
    assert!(label.contains('…'));
    assert!(maho_tui::utils::visible_width(&label) <= 25);
}
#[test]
fn single_account_has_no_suffix() {
    assert_eq!(account_footer_suffix(None, "session"), "");
}
#[test]
fn elapsed_formats_padded_larger_units() {
    for (n, out) in [
        (-1., "0s"),
        (7.9, "7s"),
        (59., "59s"),
        (60., "1m 00s"),
        (427., "7m 07s"),
        (3600., "1h 00m 00s"),
        (3667., "1h 01m 07s"),
    ] {
        assert_eq!(format_working_elapsed_seconds(n), out);
    }
}
#[test]
fn large_sessions_use_requested_cadence() {
    assert_eq!(large_session_working_status_interval(999, 32, 1000), 32);
    assert_eq!(large_session_working_status_interval(1000, 32, 1000), 1000);
    assert_eq!(
        large_session_working_status_interval(10000, 600, 60000),
        60000
    );
}
#[test]
fn active_tool_label_sanitizes_and_bounds() {
    let p = format_active_tool_working_label(
        "\x1b[31mbash\x1b]0;owned\x07\nnext",
        &json!({"command":format!("printf '{}'\nrm -rf /tmp/example","x".repeat(120))}),
    );
    assert_eq!(
        p,
        format!("Running bash next: printf '{}...", "x".repeat(50))
    );
    assert!(p.chars().count() <= 80);
}
#[test]
fn shimmer_blend_matches_highlight_to_base_order() {
    let high = WorkingStatusRgbColor {
        r: 110.,
        g: 120.,
        b: 130.,
    };
    let base = WorkingStatusRgbColor {
        r: 10.,
        g: 20.,
        b: 30.,
    };
    assert_eq!(blend_working_status_shimmer_rgb_color(high, base, 0.), base);
    assert_eq!(
        blend_working_status_shimmer_rgb_color(high, base, 0.25),
        WorkingStatusRgbColor {
            r: 35.,
            g: 45.,
            b: 55.
        }
    );
    assert_eq!(blend_working_status_shimmer_rgb_color(high, base, 1.), high);
}
#[test]
fn shimmer_sweep_repeats_after_two_seconds() {
    let base = |s: &str| format!("base({s})");
    let glow = |s: &str| format!("glow({s})");
    let highlight = |s: &str| format!("highlight({s})");
    let style = WorkingStatusTextFrameStyle {
        base: &base,
        glow: &glow,
        highlight: &highlight,
        shimmer: None,
    };
    let first = format_working_status_text_frame("Working", 0., &style);
    assert_ne!(
        first,
        format_working_status_text_frame("Working", 1000., &style)
    );
    assert_eq!(
        first,
        format_working_status_text_frame("Working", 2000., &style)
    );
}
#[test]
fn progress_reads_valid_and_rejects_wrong_types() {
    let p = read_tool_progress(
        &json!({"progress":{"activity":"waiting","startedAt":1000,"maxWaitMs":300000}}),
    )
    .expect("valid test fixture and successful operation");
    assert_eq!(
        format_tool_progress_line(&p, 13900., Some(4)),
        "⠼ waiting · 12s / max 5m 00s"
    );
    assert!(read_tool_progress(&json!({"progress":{"startedAt":"soon"}})).is_none());
    assert!(read_tool_progress(&Value::Null).is_none());
}
#[test]
fn environment_background_uses_last_valid_index() {
    assert_eq!(
        detect_terminal_background_from_env(Some("0;7;15")).theme,
        TerminalTheme::Light
    );
    assert_eq!(
        detect_terminal_background_from_env(Some("15;0")).theme,
        TerminalTheme::Dark
    );
}
#[test]
fn missing_background_hint_defaults_dark() {
    let d = detect_terminal_background_from_env(None);
    assert_eq!(d.theme, TerminalTheme::Dark);
    assert!(!d.high_confidence);
}
#[test]
fn rgb_detection_uses_luminance() {
    assert_eq!(get_theme_for_rgb_color([8; 3]), TerminalTheme::Dark);
    assert_eq!(get_theme_for_rgb_color([250; 3]), TerminalTheme::Light);
}
#[test]
fn theme_setting_pairs_parse_and_resolve() {
    assert_eq!(
        parse_auto_theme_setting(Some("light/dark")),
        Some(("light", "dark"))
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark"), TerminalTheme::Light),
        Some("light")
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark/extra"), TerminalTheme::Dark),
        None
    );
}
#[test]
fn color_mode_uses_palette_or_truecolor() {
    assert!(
        Theme::builtin("dark", ColorMode::Color256)
            .expect("valid test fixture and successful operation")
            .get_fg_ansi(ThemeColor::Accent)
            .starts_with("\x1b[38;5;")
    );
    assert!(
        theme()
            .get_fg_ansi(ThemeColor::Accent)
            .starts_with("\x1b[38;2;")
    );
}
fn dark_document() -> Value {
    serde_json::from_str(include_str!("../src/theme/dark.json")).expect("valid test fixture and successful operation")
}
#[test]
fn optional_scrollbar_colors_fall_back() {
    let mut doc = dark_document();
    doc["colors"]
        .as_object_mut()
        .expect("valid test fixture and successful operation")
        .remove("scrollbarTrack");
    doc["colors"]
        .as_object_mut()
        .expect("valid test fixture and successful operation")
        .remove("scrollbarThumb");
    let t = Theme::from_json(
        validate_theme_json("test", doc).expect("valid test fixture and successful operation"),
        ColorMode::Truecolor,
    )
    .expect("valid test fixture and successful operation");
    assert_eq!(
        t.get_fg_ansi(ThemeColor::ScrollbarTrack),
        t.get_fg_ansi(ThemeColor::Muted)
    );
    assert_eq!(
        t.get_fg_ansi(ThemeColor::ScrollbarThumb),
        t.get_fg_ansi(ThemeColor::Text)
    );
}
#[test]
fn optional_search_colors_fall_back() {
    let mut doc = dark_document();
    doc["colors"]
        .as_object_mut()
        .expect("valid test fixture and successful operation")
        .remove("searchMatchBg");
    doc["colors"]
        .as_object_mut()
        .expect("valid test fixture and successful operation")
        .remove("searchMatchText");
    let t = Theme::from_json(
        validate_theme_json("test", doc).expect("valid test fixture and successful operation"),
        ColorMode::Truecolor,
    )
    .expect("valid test fixture and successful operation");
    assert_eq!(
        t.get_bg_ansi(ThemeBg::SearchMatchBg),
        t.get_bg_ansi(ThemeBg::SelectedBg)
    );
    assert_eq!(
        t.get_fg_ansi(ThemeColor::SearchMatchText),
        t.get_fg_ansi(ThemeColor::Text)
    );
}
#[test]
fn explicit_search_colors_are_used() {
    let mut doc = dark_document();
    doc["colors"]["searchMatchBg"] = json!("#112233");
    doc["colors"]["searchMatchText"] = json!("#223344");
    let t = Theme::from_json(
        validate_theme_json("test", doc).expect("valid test fixture and successful operation"),
        ColorMode::Truecolor,
    )
    .expect("valid test fixture and successful operation");
    assert_eq!(t.get_bg_ansi(ThemeBg::SearchMatchBg), "\x1b[48;2;17;34;51m");
    assert_eq!(
        t.get_fg_ansi(ThemeColor::SearchMatchText),
        "\x1b[38;2;34;51;68m"
    );
}
#[test]
fn export_resolves_recursive_vars_and_palette_indices() {
    let mut doc = dark_document();
    doc["vars"]["page"] = json!("#abcdef");
    doc["vars"]["alias"] = json!("page");
    doc["export"] = json!({"pageBg":"alias","cardBg":24,"infoBg":""});
    let doc: ThemeJson = serde_json::from_value(doc).expect("valid test fixture and successful operation");
    let colors = get_theme_export_colors(&doc).expect("valid test fixture and successful operation");
    assert_eq!(colors["pageBg"], "#abcdef");
    assert_eq!(colors["cardBg"], "#005f87");
    assert!(!colors.contains_key("infoBg"));
}
#[test]
fn stale_detection_cannot_replace_explicit_selection() {
    use maho_interactive::theme::theme_controller::InteractiveThemeController;
    let mut c = InteractiveThemeController::new(
        Some("light/dark".into()),
        TerminalTheme::Dark,
        ColorMode::Truecolor,
    )
    .expect("valid test fixture and successful operation");
    let generation = c.apply_from_settings().expect("valid test fixture and successful operation");
    c.set_theme_name("dark").expect("valid test fixture and successful operation");
    assert!(
        !c.resolve_detection(generation, TerminalTheme::Light)
            .expect("valid test fixture and successful operation")
    );
    assert_eq!(c.active_theme.name, "dark");
}
