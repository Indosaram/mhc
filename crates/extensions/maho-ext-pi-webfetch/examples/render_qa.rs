use maho_ext_api::{AgentToolResult, Theme, ToolRenderResultOptions};
use maho_ext_pi_webfetch::webfetch::renderers::{render_call, render_result};
use maho_tui::tui::Component;
use serde_json::json;

fn main() {
    let theme = Theme {
        colors: [
            ("accent".into(), "\u{1b}[38;2;125;207;255m".into()),
            ("success".into(), "\u{1b}[38;2;158;206;106m".into()),
            ("warning".into(), "\u{1b}[38;2;224;175;104m".into()),
            ("muted".into(), "\u{1b}[38;2;169;177;214m".into()),
            ("dim".into(), "\u{1b}[38;2;86;95;137m".into()),
            ("toolTitle".into(), "\u{1b}[38;2;187;154;247m".into()),
            ("toolOutput".into(), "\u{1b}[38;2;192;202;245m".into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    for width in [100, 45] {
        println!("\r\nwebfetch renderer width={width}");
        let mut call = render_call(
            &json!({"url":"https://example.test/티스토리/본문","format":"markdown","timeout":7}),
            &theme,
        );
        for line in call.render(width) {
            println!("{line}\r");
        }
        let mut result = AgentToolResult::text(
            "# 티스토리 본문\n\n첫 번째 본문 문장과 Alpha **Beta**\n두 번째 본문 문장",
        );
        result.details = json!({"status":200,"statusText":"OK","format":"markdown","bytes":2048,"converted":true,"outputTruncated":false,
            "finalUrl":"https://example.test/티스토리/본문","contentType":"text/html"});
        for expanded in [false, true] {
            let mut output = render_result(
                &result,
                ToolRenderResultOptions {
                    expanded,
                    is_partial: false,
                },
                &theme,
            );
            for line in output.render(width) {
                println!("{line}\r");
            }
        }
    }
}
