use maho_ext_api::{AgentToolResult, Theme, ToolRenderResultOptions};
use maho_ext_pi_webfetch::webfetch::renderers::{render_call, render_result};
use maho_tui::tui::Component;
use serde_json::json;

fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut escape = false;
    for ch in text.chars() {
        if ch == '\u{1b}' {
            escape = true;
        } else if escape && ch == 'm' {
            escape = false;
        } else if !escape {
            output.push(ch);
        }
    }
    output.trim_end().to_owned()
}

#[test]
fn call_renderer_uses_params() {
    let mut call = render_call(
        &json!({"url":"https://example.test/page","format":"text","timeout":7}),
        &Theme::default(),
    );
    let lines = call.render(200);
    assert_eq!(
        strip_ansi(&lines[0]),
        "webfetch https://example.test/page [text] 7s"
    );
}
#[test]
fn progress_renderer_reads_details() {
    let mut result = AgentToolResult::text("unrelated text");
    result.details =
        json!({"phase":"fetching","url":"http://fixture/page","format":"html","timeoutSeconds":5});
    let mut output = render_result(
        &result,
        ToolRenderResultOptions {
            expanded: false,
            is_partial: true,
        },
        &Theme::default(),
    );
    assert_eq!(
        strip_ansi(&output.render(200)[0]),
        "Fetching http://fixture/page as html (5s)"
    );
}
#[test]
fn final_renderer_preview_and_expansion() {
    let mut result = AgentToolResult::text("one\n\ntwo\nthree\nfour\nfive");
    result.details = json!({"status":200,"statusText":"Custom","format":"text","bytes":1024,"converted":true,
        "outputTruncated":true,"finalUrl":"http://fixture/final","contentType":"text/plain"});
    let mut collapsed = render_result(
        &result,
        ToolRenderResultOptions::default(),
        &Theme::default(),
    );
    let lines = collapsed
        .render(200)
        .iter()
        .map(|line| strip_ansi(line))
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            "200 Custom \u{2022} text \u{2022} 1.0 KB converted truncated",
            "  one",
            "  two",
            "  three",
            "  four"
        ]
    );
    let mut expanded = render_result(
        &result,
        ToolRenderResultOptions {
            expanded: true,
            is_partial: false,
        },
        &Theme::default(),
    );
    let lines = expanded
        .render(200)
        .iter()
        .map(|line| strip_ansi(line))
        .collect::<Vec<_>>();
    assert_eq!(lines[1], "URL: http://fixture/final");
    assert_eq!(lines[2], "Content-Type: text/plain");
    assert_eq!(lines.last().expect("body"), "five");
}
#[test]
fn renderer_applies_exported_theme_prefixes_and_default_ellipsis() {
    let theme = Theme { colors: [("accent".to_string(), "\u{1b}[38;5;117m".to_string())].into_iter().collect(), ..Default::default() };
    let mut call = render_call(&json!({"url":"https://example.test/page"}), &theme);
    assert!(call.render(200)[0].contains("\u{1b}[38;5;117mhttps://example.test/page\u{1b}[39m"));
    let mut result = AgentToolResult::text("x".repeat(130));
    result.details = json!({});
    let mut output = render_result(&result, ToolRenderResultOptions::default(), &Theme::default());
    assert_eq!(strip_ansi(&output.render(200)[0]), format!("{}...", "x".repeat(117)));
}
#[test]
fn remaining_result_branches_follow_source() {
    let mut partial = AgentToolResult::text("ignored");
    partial.details = json!({"phase":"downloading","url":"http://fixture/page"});
    let mut output = render_result(
        &partial,
        ToolRenderResultOptions {
            expanded: false,
            is_partial: true,
        },
        &Theme::default(),
    );
    assert_eq!(strip_ansi(&output.render(200)[0]), "Fetching...");
    let mut plain = AgentToolResult::text("plain error text");
    plain.details = json!({"status":"404"});
    let mut output = render_result(
        &plain,
        ToolRenderResultOptions::default(),
        &Theme::default(),
    );
    assert_eq!(strip_ansi(&output.render(200)[0]), "plain error text");
    let mut empty = AgentToolResult::text(" \n\t\n");
    empty.details = json!({"status":404,"statusText":"","format":"html","bytes":2097152,"converted":false,"outputTruncated":false});
    let mut output = render_result(
        &empty,
        ToolRenderResultOptions::default(),
        &Theme::default(),
    );
    let lines = output
        .render(200)
        .iter()
        .map(|line| strip_ansi(line))
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        ["404 OK \u{2022} html \u{2022} 2.0 MB", "  empty response"]
    );
    let mut expanded = render_result(
        &empty,
        ToolRenderResultOptions {
            expanded: true,
            is_partial: false,
        },
        &Theme::default(),
    );
    let lines = expanded
        .render(200)
        .iter()
        .map(|line| strip_ansi(line))
        .collect::<Vec<_>>();
    assert_eq!(lines[2], "Content-Type: unknown");
}
