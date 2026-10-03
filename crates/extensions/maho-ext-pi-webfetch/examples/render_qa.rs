use maho_ext_api::{AgentToolResult, Theme, ToolRenderResultOptions};
use maho_ext_pi_webfetch::webfetch::renderers::{render_call, render_result};
use maho_tui::tui::Component;
use serde_json::json;

fn main() {
    let theme = Theme { colors: [("accent".into(), "#7dcfff".into()), ("success".into(), "#9ece6a".into()),
        ("warning".into(), "#e0af68".into()), ("muted".into(), "#a9b1d6".into()), ("dim".into(), "#565f89".into()),
        ("toolTitle".into(), "#bb9af7".into()), ("toolOutput".into(), "#c0caf5".into())].into_iter().collect(), ..Default::default() };
    for width in [100, 45] {
        println!("\r\nwebfetch renderer width={width}");
        let mut call = render_call(&json!({"url":"https://example.test/티스토리/본문","format":"markdown","timeout":7}), &theme);
        for line in call.render(width) { println!("{line}\r"); }
        let mut result = AgentToolResult::text("# 티스토리 본문\n\n첫 번째 본문 문장과 Alpha **Beta**\n두 번째 본문 문장");
        result.details = json!({"status":200,"statusText":"OK","format":"markdown","bytes":2048,"converted":true,"outputTruncated":false,
            "finalUrl":"https://example.test/티스토리/본문","contentType":"text/html"});
        for expanded in [false, true] {
            let mut output = render_result(&result, ToolRenderResultOptions { expanded, is_partial: false }, &theme);
            for line in output.render(width) { println!("{line}\r"); }
        }
    }
}
