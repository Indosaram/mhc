use maho_ext_api::{AgentToolResult, ContentBlock, Theme, ToolRenderers, ToolRenderResultOptions};
use maho_tui::{components::text::Text, utils::truncate_to_width};
use serde_json::Value;
use std::sync::Arc;

fn fg(theme: &Theme, key: &str, text: &str) -> String {
    let color = theme.colors.get(key).map(String::as_str).unwrap_or("");
    let prefix = if let Some(hex) = color.strip_prefix('#').filter(|hex| hex.len() == 6) {
        match u32::from_str_radix(hex, 16) {
            Ok(rgb) => format!("\x1b[38;2;{};{};{}m", (rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255),
            Err(_) => String::new(),
        }
    } else { "\x1b[39m".into() };
    format!("{prefix}{text}\x1b[39m")
}
fn shorten(value: &str, max: usize) -> String {
    let units = value.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max { value.into() } else { format!("{}\u{2026}", String::from_utf16_lossy(&units[..max - 1])) }
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str { value[key].as_str().unwrap_or("") }
fn clipped(text: &str) -> String { truncate_to_width(text, 120, "\u{2026}", false) }
fn bytes(bytes: u64) -> String {
    if bytes < 1024 { return format!("{bytes} B"); }
    let value = maho_ai::utils::js::string_to_number(&bytes.to_string());
    if bytes < 1024 * 1024 { format!("{:.1} KB", value / 1024.0) } else { format!("{:.1} MB", value / (1024.0 * 1024.0)) }
}
pub fn render_call(args: &Value, theme: &Theme) -> Text {
    let head = fg(theme, "toolTitle", "\x1b[1mwebfetch \x1b[22m");
    let url = fg(theme, "accent", &shorten(string(args, "url"), 92));
    let format = fg(theme, "muted", &format!(" [{}]", args["format"].as_str().unwrap_or("markdown")));
    let timeout = args["timeout"].as_f64().map_or_else(String::new, |timeout| fg(theme, "dim", &format!(" {}s", maho_ai::utils::js::number_to_string(timeout))));
    Text::with_padding(format!("{head}{url}{format}{timeout}"), 0, 0)
}
pub fn render_result(result: &AgentToolResult, options: ToolRenderResultOptions, theme: &Theme) -> Text {
    let details = &result.details;
    let text = result.content.iter().find_map(|block| match block { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).unwrap_or("");
    let output = if options.is_partial {
        let message = if details["phase"] == "fetching" {
            format!("Fetching {} as {} ({}s)", shorten(string(details, "url"), 92), string(details, "format"), details["timeoutSeconds"])
        } else { "Fetching...".into() };
        fg(theme, "warning", &message)
    } else if let Some(status) = details["status"].as_u64() {
        let status_key = if (200..300).contains(&status) { "success" } else { "warning" };
        let reason = details["statusText"].as_str().filter(|text| !text.is_empty()).unwrap_or("OK");
        let converted = if details["converted"] == true { fg(theme, "dim", " converted") } else { String::new() };
        let truncated = if details["outputTruncated"] == true { fg(theme, "warning", " truncated") } else { String::new() };
        let mut lines = vec![format!("{} {} {} {} {}{converted}{truncated}", fg(theme, status_key, &format!("{status} {reason}")),
            fg(theme, "muted", "\u{2022}"), fg(theme, "accent", string(details, "format")), fg(theme, "muted", "\u{2022}"),
            fg(theme, "muted", &bytes(details["bytes"].as_u64().unwrap_or(0))))];
        if options.expanded {
            lines.push(fg(theme, "dim", &format!("URL: {}", shorten(string(details, "finalUrl"), 92))));
            lines.push(fg(theme, "dim", &format!("Content-Type: {}", details["contentType"].as_str().filter(|text| !text.is_empty()).unwrap_or("unknown"))));
            lines.push(String::new());
            lines.extend(text.split('\n').take(24).map(|line| fg(theme, "toolOutput", &clipped(line))));
        } else {
            let preview = text.split('\n').map(str::trim).filter(|line| !line.is_empty()).take(4).collect::<Vec<_>>();
            if preview.is_empty() { lines.push(fg(theme, "dim", "  empty response")); }
            else { lines.extend(preview.iter().map(|line| fg(theme, "toolOutput", &format!("  {}", clipped(line))))); }
        }
        lines.join("\n")
    } else { fg(theme, "muted", &clipped(text)) };
    Text::with_padding(output, 0, 0)
}
pub fn renderers() -> ToolRenderers<(), Value> {
    ToolRenderers { render_call: Some(Arc::new(|args, theme, _| Box::new(render_call(args, theme)))),
        render_result: Some(Arc::new(|result, options, theme, _| Box::new(render_result(result, options, theme)))) }
}
