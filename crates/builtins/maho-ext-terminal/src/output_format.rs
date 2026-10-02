use maho_ai::types::{TextAudience, TextContent};
use maho_tools::truncate::{TruncationOptions, TruncationResult, format_size, truncate_tail};
use std::sync::LazyLock;

pub const TERMINAL_TOOL_MAX_LINES: usize = 2000;
pub const TERMINAL_TOOL_MAX_BYTES: usize = 50 * 1024;

static ESCAPE_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    [r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", r"\x1b\[[0-?]*[ -/]*[@-~]", r"\x1b(?:[()#%*+][0-9A-Za-z]|[0-9<=>@-Z\\-_])", r"[\x00-\x07\x0b\x0c\x0e-\x1f\x7f]"]
        .into_iter().map(|pattern| {
            #[expect(clippy::expect_used, reason = "constant escape patterns are validated by sanitizer tests")]
            regex::Regex::new(pattern).expect("valid terminal escape pattern")
        }).collect()
});

fn collapse_redraws(line: &str) -> String {
    let mut cells = Vec::new(); let mut cursor: usize = 0;
    for ch in line.chars() {
        match ch {
            '\r' => cursor = 0,
            '\x08' => cursor = cursor.saturating_sub(1),
            ch => { if cursor == cells.len() { cells.push(ch); } else { cells[cursor] = ch; } cursor += 1; }
        }
    }
    cells.into_iter().collect()
}

pub fn sanitize_terminal_output(raw: &str) -> String {
    let mut text = raw.to_owned();
    for pattern in ESCAPE_PATTERNS.iter() { text = pattern.replace_all(&text, "").into_owned(); }
    text.replace("\r\n", "\n").split('\n').map(collapse_redraws).collect::<Vec<_>>().join("\n")
}

pub struct FormattedTerminalToolOutput {
    pub text: String,
    pub body: String,
    pub marker: Option<String>,
    pub truncated: bool,
    pub truncation: TruncationResult,
}

pub fn format_terminal_tool_output(raw: &str) -> FormattedTerminalToolOutput {
    let sanitized = sanitize_terminal_output(raw).trim_end().to_owned();
    let truncation = truncate_tail(&sanitized, TruncationOptions { max_lines: TERMINAL_TOOL_MAX_LINES, max_bytes: TERMINAL_TOOL_MAX_BYTES });
    if !truncation.truncated { return FormattedTerminalToolOutput { text: sanitized.clone(), body: sanitized, marker: None, truncated: false, truncation }; }
    let marker = if truncation.last_line_partial {
        format!("[Showing last {} of a single line; earlier output dropped]", format_size(truncation.output_bytes))
    } else {
        format!("[Showing lines {}-{} of {}; earlier output dropped]", truncation.total_lines-truncation.output_lines+1, truncation.total_lines, truncation.total_lines)
    };
    FormattedTerminalToolOutput { text: format!("{}\n\n{marker}",truncation.content), body: truncation.content.clone(), marker: Some(marker), truncated: true, truncation }
}

fn text_part(text: &str, audience: Option<TextAudience>) -> TextContent {
    TextContent { text: text.to_owned(), audience, text_signature: None }
}

pub fn split_model_only_notices(text: &str, notices: &[Option<&str>]) -> Vec<TextContent> {
    let mut parts = Vec::new(); let mut cursor = 0; let mut after_notice = false;
    for notice in notices.iter().flatten().filter(|value| !value.is_empty()) {
        let Some(relative) = text[cursor..].find(notice) else { continue; };
        let index = cursor + relative; let end = index + notice.len();
        if end < text.len() && text.as_bytes()[end] != b'\n' { continue; }
        if after_notice && index == cursor {
            parts.push(text_part(notice,Some(TextAudience::Model)));
        } else if index > cursor && text.as_bytes()[index-1] == b'\n' {
            parts.push(text_part(&text[cursor..index-1],None)); parts.push(text_part(notice,Some(TextAudience::Model)));
        } else { continue; }
        cursor = if end < text.len() { end+1 } else { end }; after_notice = end < text.len();
    }
    if parts.is_empty() { return vec![text_part(text,None)]; }
    if cursor < text.len() || text.ends_with('\n') { parts.push(text_part(&text[cursor..],None)); }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn plain_text_passthrough() { assert_eq!(sanitize_terminal_output("hello world\nsecond line"),"hello world\nsecond line"); }
    #[test] fn strips_csi_colors_and_cursor() { assert_eq!(sanitize_terminal_output("\x1b[1m\x1b[31mred bold\x1b[0m plain"),"red bold plain");assert_eq!(sanitize_terminal_output("\x1b[2m$\x1b[0m \x1b[1mls\x1b[0m"),"$ ls"); }
    #[test] fn strips_private_modes_and_keypad() { assert_eq!(sanitize_terminal_output("\x1b[?1h\x1b=\r\nok\x1b[?1l\x1b>"),"\nok"); }
    #[test] fn strips_osc_hyperlinks_and_titles() { assert_eq!(sanitize_terminal_output("\x1b]8;;https://example.com\x07link\x1b]8;;\x07"),"link");assert_eq!(sanitize_terminal_output("\x1b]11;?\x1b\\done"),"done"); }
    #[test] fn normalizes_crlf() { assert_eq!(sanitize_terminal_output("one\r\ntwo\r\nthree"),"one\ntwo\nthree"); }
    #[test] fn final_spinner_frame_survives() { assert_eq!(sanitize_terminal_output("\r\x1b[K\x1b[36m\u{28fe}\x1b[0m\r\x1b[K\x1b[36m\u{28fd}\x1b[0m\r\x1b[K\x1b[36m\u{28fb}\x1b[0m done"),"\u{28fb} done"); }
    #[test] fn shorter_redraw_keeps_tail() { assert_eq!(sanitize_terminal_output("abcdef\rXY"),"XYcdef"); }
    #[test] fn carriage_return_per_line() { assert_eq!(sanitize_terminal_output("first-aaa\rfirst-b\nsecond-ccc\rsecond-d"),"first-baa\nsecond-dcc"); }
    #[test] fn backspace_overwrites() { assert_eq!(sanitize_terminal_output("abc\x08\x08XY"),"aXY"); }
    #[test] fn controls_removed_tabs_kept() { assert_eq!(sanitize_terminal_output("a\x00b\x07c\td"),"abc\td"); }
    #[test] fn within_budget_sanitizes() { let result=format_terminal_tool_output("\x1b[32mok\x1b[0m\n");assert!(!result.truncated);assert_eq!(result.text,"ok"); }
    #[test] fn line_budget_keeps_tail() { let raw=(0..2500).map(|i|format!("line-{i}")).collect::<Vec<_>>().join("\n");let result=format_terminal_tool_output(&raw);assert!(result.truncated);assert_eq!(result.truncation.output_lines,2000);assert!(result.body.starts_with("line-500\n"));assert!(result.body.ends_with("line-2499")); }
    #[test] fn byte_budget_bounds_output() { let result=format_terminal_tool_output(&"x".repeat(TERMINAL_TOOL_MAX_BYTES*4));assert!(result.truncated);assert!(result.text.len()<=TERMINAL_TOOL_MAX_BYTES+512); }
    #[test] fn sanitize_before_truncating() { let frame="\r\x1b[K\x1b[36m\u{28fe}\x1b[0m";let result=format_terminal_tool_output(&format!("{}\r\x1b[K\x1b[32m\u{2713}\x1b[0m finished",frame.repeat(3000)));assert!(!result.truncated);assert_eq!(result.text,"\u{2713} finished"); }
    #[test] fn marker_separate_from_body() { let raw=(0..2010).map(|i|format!("line-{i}")).collect::<Vec<_>>().join("\n");let result=format_terminal_tool_output(&raw);assert!(result.marker.is_some());assert_eq!(result.text,format!("{}\n\n{}",result.body,result.marker.expect("marker"))); }
    #[test] fn trailing_notice_is_model_only() { let text="out-a\nout-b\n\n[marker]";let parts=split_model_only_notices(text,&[Some("[marker]")]);assert_eq!(parts,vec![text_part("out-a\nout-b\n",None),text_part("[marker]",Some(TextAudience::Model))]);assert_eq!(parts.iter().map(|p|p.text.as_str()).collect::<Vec<_>>().join("\n"),text); }
    #[test] fn leading_and_trailing_notices_rejoin_exactly() { let text="status\n[drop]\nbody\n\n[marker]";let parts=split_model_only_notices(text,&[Some("[drop]"),Some("[marker]")]);assert_eq!(parts.iter().map(|p|p.audience).collect::<Vec<_>>(),[None,Some(TextAudience::Model),None,Some(TextAudience::Model)]);assert_eq!(parts.iter().map(|p|p.text.as_str()).collect::<Vec<_>>().join("\n"),text); }
    #[test] fn no_applicable_notice_keeps_whole_text() { assert_eq!(split_model_only_notices("plain output",&[None]),vec![text_part("plain output",None)]);assert_eq!(split_model_only_notices("inline [marker] text",&[Some("[marker]")]),vec![text_part("inline [marker] text",None)]); }
}
