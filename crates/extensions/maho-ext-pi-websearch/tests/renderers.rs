use maho_ext_pi_websearch::websearch::{renderers::*, tool::SearchParams, types::*};
use maho_interactive::theme::{
    ColorMode, Theme,
    theme_json::{ColorValue, ThemeJson},
};
use maho_tui::tui::Component;
#[test]
fn registered_renderer_preserves_typed_details_and_live_theme_colors() {
    use maho_ext_api::{
        AgentToolResult, ToolRenderContext, ToolRenderResultOptions, ToolRendererSession,
    };
    use std::{rc::Rc, sync::Arc};
    let renderers = Arc::new(registered_renderers());
    let mut context = ToolRenderContext {
        args: serde_json::json!({"query":"manual docs"}),
        tool_call_id: "qa".into(),
        invalidate: Rc::new(|| {}),
        last_component: None,
        state: (),
        cwd: "/fixture".into(),
        execution_started: false,
        args_complete: true,
        is_partial: false,
        expanded: false,
        show_images: false,
        image_protocol: None,
        is_error: false,
        has_result: None,
        spinner_frame: None,
    };
    let colors = [
        "toolTitle",
        "accent",
        "muted",
        "dim",
        "error",
        "success",
        "warning",
    ]
    .into_iter()
    .map(|key| (key.into(), "#123456".into()))
    .collect();
    let palette = maho_ext_api::Theme {
        colors,
        ..Default::default()
    };
    let mut call = (renderers.render_call.as_ref().expect("call"))(
        &context.args.clone(),
        &palette,
        &mut context,
    );
    assert!(call.render(80)[0].contains("\x1b[38;2;18;52;86m"));
    let mut error = AgentToolResult::text("do not parse this text");
    error.details =
        serde_json::json!({"phase":"error","query":"manual docs","error":"typed sentinel"});
    let mut result = (renderers.render_result.as_ref().expect("result"))(
        &error,
        ToolRenderResultOptions::default(),
        &palette,
        &mut context,
    );
    assert!(result.render(80)[0].contains("typed sentinel"));
    let _session = ToolRendererSession { renderers, context };
}
#[test]
fn progress_numeric_label_uses_javascript_infinity() {
    let theme = theme();
    let progress = SearchRenderDetails::Progress(SearchProgressDetails {
        phase: SearchingPhase::Searching,
        query: "q".into(),
        provider_labels: Vec::new(),
        max_results: f64::INFINITY,
        current_provider: None,
        attempts: None,
        route_labels: None,
        strategy: None,
        allowed_domains: None,
        blocked_domains: None,
    });
    let mut text = render_search_result(
        None,
        Some(&progress),
        &RenderResultOptions {
            expanded: false,
            is_partial: true,
        },
        &theme,
    );
    assert!(text.render(200)[0].contains("Infinity"));
}
fn theme() -> Theme {
    let colors = [
        ("toolTitle", 6),
        ("accent", 6),
        ("muted", 7),
        ("dim", 8),
        ("error", 1),
        ("success", 2),
        ("warning", 3),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), ColorValue::Index(value)))
    .collect();
    Theme::from_json(
        ThemeJson {
            appearance: None,
            name: "fixture".into(),
            vars: Default::default(),
            colors,
            export_colors: Default::default(),
        },
        ColorMode::Truecolor,
    )
    .expect("theme")
}
#[test]
fn native_call_has_zero_padding_and_allowed_filter_precedence() {
    let theme = theme();
    let mut call = render_search_call(
        &SearchParams {
            query: "q".into(),
            allowed_domains: Some(Vec::new()),
            blocked_domains: Some(vec!["example.org".into()]),
        },
        &theme,
    );
    let lines = call.render(200);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("\u{1b}[38;5;6m\u{1b}[1m"));
    assert!(!lines[0].contains("domains:"));
}
#[test]
fn typed_errors_and_absent_details_choose_native_palette() {
    use maho_interactive::theme::ThemeColor;
    let theme = theme();
    let error = SearchRenderDetails::Error(SearchErrorDetails {
        phase: ErrorPhase::Error,
        query: "q".into(),
        error: "sentinel".into(),
        reason: None,
    });
    let mut text =
        render_search_result(None, Some(&error), &RenderResultOptions::default(), &theme);
    assert_eq!(
        text.render(8),
        vec![theme.fg(ThemeColor::Error, "sentinel")]
    );
    let mut fallback = render_search_result(
        Some("sentinel"),
        None,
        &RenderResultOptions::default(),
        &theme,
    );
    assert_eq!(
        fallback.render(8),
        vec![theme.fg(ThemeColor::Muted, "sentinel")]
    );
}
#[test]
fn progress_expansion_only_adds_route_for_current_provider() {
    let theme = theme();
    let mut progress = SearchProgressDetails {
        phase: SearchingPhase::Searching,
        query: "q".into(),
        provider_labels: vec!["exa".into()],
        max_results: 5.0,
        current_provider: None,
        attempts: None,
        route_labels: None,
        strategy: None,
        allowed_domains: None,
        blocked_domains: None,
    };
    let options = RenderResultOptions {
        expanded: true,
        is_partial: true,
    };
    let mut text = render_search_result(
        None,
        Some(&SearchRenderDetails::Progress(progress.clone())),
        &options,
        &theme,
    );
    assert_eq!(text.render(200).len(), 1);
    progress.current_provider = Some("exa".into());
    let mut text = render_search_result(
        None,
        Some(&SearchRenderDetails::Progress(progress)),
        &options,
        &theme,
    );
    assert_eq!(text.render(200).len(), 2);
}

mod upstream_renderer_suite {
    use super::theme;
    use maho_ext_pi_websearch::websearch::{renderers::*, tool::SearchParams, types::*};
    use maho_tui::tui::Component;

    fn plain(text: &str) -> String {
        maho_tui::utils::strip_terminal_sequences(text)
    }
    fn render(details: serde_json::Value, expanded: bool, is_partial: bool) -> String {
        let details: SearchRenderDetails = serde_json::from_value(details).expect("details");
        let mut text = render_search_result(
            Some("ok"),
            Some(&details),
            &RenderResultOptions {
                expanded,
                is_partial,
            },
            &theme(),
        );
        plain(&text.render(120).join("\n"))
    }
    fn has_step_counter(text: &str) -> bool {
        text.split('[').skip(1).any(|rest| {
            let Some((inner, _)) = rest.split_once(']') else {
                return false;
            };
            let Some((left, right)) = inner.split_once('/') else {
                return false;
            };
            !left.is_empty()
                && !right.is_empty()
                && left
                    .bytes()
                    .chain(right.bytes())
                    .all(|byte| byte.is_ascii_digit())
        })
    }
    fn route_progress() -> serde_json::Value {
        serde_json::json!({"phase":"searching","query":"route states",
            "providerLabels":["exa/primary","duckduckgo-html/backup","brave/extra"],
            "routeLabels":["exa/primary","duckduckgo-html/backup","brave/extra"],
            "currentProvider":"duckduckgo-html/backup",
            "attempts":[{"provider":"exa","entryId":"primary","durationMs":100,"resultsCount":0,"error":"boom"}],
            "maxResults":10})
    }
    fn native_details() -> serde_json::Value {
        serde_json::json!({"provider":"openai","entryId":"native-openai-abc123","query":"native label",
            "results":[{"title":"Native","url":"https://example.com/native","snippet":"snip"}],
            "durationMs":7,"truncated":false,"strategy":"priority",
            "attempts":[{"provider":"openai","entryId":"native-openai-abc123","durationMs":7,"resultsCount":1}]})
    }

    #[test]
    fn call_includes_tool_and_query() {
        let args = SearchParams {
            query: "pi extensions".into(),
            allowed_domains: Some(vec!["example.com".into()]),
            blocked_domains: None,
        };
        let rendered = plain(&render_search_call(&args, &theme()).render(80).join("\n"));
        assert!(
            rendered.contains("web_search") && rendered.contains("pi extensions"),
            "{rendered}"
        );
    }

    #[test]
    fn partial_progress_includes_route_and_limit() {
        let rendered = render(
            serde_json::json!({"phase":"searching","query":"pi extensions","providerLabels":["default/duckduckgo-html"],"maxResults":10}),
            false,
            true,
        );
        for needle in ["Searching", "default/duckduckgo-html", "max 10"] {
            assert!(rendered.contains(needle), "{needle}: {rendered}");
        }
    }

    #[test]
    fn expanded_and_collapsed_results_include_source_rows() {
        let expanded = render(
            serde_json::json!({"provider":"exa","entryId":"exa-search","query":"pi extensions",
                "results":[{"title":"Pi","url":"https://example.com/pi","snippet":"Pi docs"}],"durationMs":42,"truncated":false,
                "strategy":"priority","attempts":[{"provider":"exa","entryId":"exa-search","durationMs":42,"resultsCount":1}]}),
            true,
            false,
        );
        for needle in [
            "1 result",
            "exa/exa-search",
            "route exa/exa-search:1",
            "https://example.com/pi",
        ] {
            assert!(expanded.contains(needle), "{needle}: {expanded}");
        }
        let collapsed = render(
            serde_json::json!({"provider":"exa","entryId":"exa-search","query":"pi extensions",
                "results":[{"title":"Pi","url":"https://example.com/pi","snippet":"Pi docs"},{"title":"Extensions","url":"https://example.com/extensions"}],
                "durationMs":42,"truncated":false,"strategy":"priority"}),
            false,
            false,
        );
        for needle in ["2 results", "Pi", "https://example.com/pi", "Pi docs"] {
            assert!(collapsed.contains(needle), "{needle}: {collapsed}");
        }
    }

    #[test]
    fn error_details_display_message() {
        let rendered = render(
            serde_json::json!({"phase":"error","query":"pi extensions","error":"Invalid provider config"}),
            false,
            false,
        );
        assert!(rendered.contains("Invalid provider config"), "{rendered}");
    }

    #[test]
    fn route_progress_collapsed_shows_current_provider_and_expanded_adds_three_state_route() {
        let collapsed = render(route_progress(), false, true);
        assert_eq!(
            collapsed.trim(),
            "Searching \"route states\" via duckduckgo-html/backup (max 10)"
        );
        assert!(!has_step_counter(&collapsed));
        let expanded = render(route_progress(), true, true);
        assert!(
            expanded.contains("Searching \"route states\" via duckduckgo-html/backup (max 10)"),
            "{expanded}"
        );
        assert!(
            expanded.contains("route exa/primary:failed -> duckduckgo-html/backup:searching -> brave/extra:pending"),
            "{expanded}"
        );
        assert!(!has_step_counter(&expanded));
    }

    #[test]
    fn native_entry_id_collapses_to_provider_native() {
        let collapsed = render(native_details(), false, false);
        assert!(collapsed.contains("via openai/native"), "{collapsed}");
        assert!(!collapsed.contains("native-openai-abc123"));
        let expanded = render(native_details(), true, false);
        assert!(expanded.contains("route openai/native:1"), "{expanded}");
        assert!(!expanded.contains("native-openai-abc123"));
    }
}
