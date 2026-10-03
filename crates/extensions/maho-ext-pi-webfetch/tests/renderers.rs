use maho_ext_api::{AgentToolResult, Theme, ToolRenderResultOptions};
use maho_ext_pi_webfetch::webfetch::renderers::{render_call, render_result};
use maho_tui::tui::Component;
use serde_json::json;

fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut escape = false;
    for ch in text.chars() {
        if ch == '\u{1b}' { escape = true; }
        else if escape && ch == 'm' { escape = false; }
        else if !escape { output.push(ch); }
    }
    output.trim_end().to_owned()
}

#[test]
fn call_renderer_uses_params() {
    let mut call = render_call(&json!({"url":"https://example.test/page","format":"text","timeout":7}), &Theme::default());
    let lines = call.render(200);
    assert_eq!(strip_ansi(&lines[0]), "webfetch https://example.test/page [text] 7s");
}
#[test]
fn progress_renderer_reads_details() {
    let mut result = AgentToolResult::text("unrelated text");
    result.details = json!({"phase":"fetching","url":"http://fixture/page","format":"html","timeoutSeconds":5});
    let mut output = render_result(&result, ToolRenderResultOptions { expanded: false, is_partial: true }, &Theme::default());
    assert_eq!(strip_ansi(&output.render(200)[0]), "Fetching http://fixture/page as html (5s)");
}
#[test]
fn final_renderer_preview_and_expansion() {
    let mut result = AgentToolResult::text("one\n\ntwo\nthree\nfour\nfive");
    result.details = json!({"status":200,"statusText":"Custom","format":"text","bytes":1024,"converted":true,
        "outputTruncated":true,"finalUrl":"http://fixture/final","contentType":"text/plain"});
    let mut collapsed = render_result(&result, ToolRenderResultOptions::default(), &Theme::default());
    let lines = collapsed.render(200).iter().map(|line| strip_ansi(line)).collect::<Vec<_>>();
    assert_eq!(lines, ["200 Custom \u{2022} text \u{2022} 1.0 KB converted truncated", "  one", "  two", "  three", "  four"]);
    let mut expanded = render_result(&result, ToolRenderResultOptions { expanded: true, is_partial: false }, &Theme::default());
    let lines = expanded.render(200).iter().map(|line| strip_ansi(line)).collect::<Vec<_>>();
    assert_eq!(lines[1], "URL: http://fixture/final");
    assert_eq!(lines[2], "Content-Type: text/plain");
    assert_eq!(lines.last().expect("body"), "five");
}
