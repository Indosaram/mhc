use serde::{Deserialize, Serialize};
pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<String>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}
#[derive(Clone, Copy, Debug)]
pub struct TruncationOptions { pub max_lines: usize, pub max_bytes: usize }
impl Default for TruncationOptions {
    fn default() -> Self { Self { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES } }
}
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 { format!("{bytes}B") }
    else if bytes < 1024 * 1024 { format!("{:.1}KB", bytes as f64 / 1024.0) }
    else { format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0)) }
}
fn truncate(content: &str, options: TruncationOptions, tail: bool) -> TruncationResult {
    let lines: Vec<_> = if content.is_empty() { Vec::new() } else { content.split_terminator('\n').collect() };
    let mut result = TruncationResult {
        content: content.into(), truncated: false, truncated_by: None, total_lines: lines.len(), total_bytes: content.len(),
        output_lines: lines.len(), output_bytes: content.len(), last_line_partial: false, first_line_exceeds_limit: false,
        max_lines: options.max_lines, max_bytes: options.max_bytes,
    };
    if lines.len() <= options.max_lines && content.len() <= options.max_bytes { return result; }
    result.truncated = true;
    result.truncated_by = Some("lines".into());
    let mut selected = Vec::new();
    let mut bytes = 0;
    if !tail && lines.first().is_some_and(|s| s.len() > options.max_bytes) {
        result.first_line_exceeds_limit = true;
        result.truncated_by = Some("bytes".into());
    } else {
        for index in 0..lines.len().min(options.max_lines) {
            let line = lines[if tail { lines.len() - index - 1 } else { index }];
            let size = line.len() + usize::from(!selected.is_empty());
            if bytes + size > options.max_bytes {
                result.truncated_by = Some("bytes".into());
                if tail && selected.is_empty() {
                    let mut start = line.len().saturating_sub(options.max_bytes);
                    while !line.is_char_boundary(start) { start += 1; }
                    selected.push(&line[start..]);
                    bytes = line.len() - start;
                    result.last_line_partial = true;
                }
                break;
            }
            selected.push(line);
            bytes += size;
        }
        if selected.len() >= options.max_lines && bytes <= options.max_bytes { result.truncated_by = Some("lines".into()); }
    }
    if tail { selected.reverse(); }
    result.content = selected.join("\n");
    result.output_lines = selected.len();
    result.output_bytes = result.content.len();
    result
}
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult { truncate(content, options, false) }
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult { truncate(content, options, true) }
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    let units: Vec<_> = line.encode_utf16().collect();
    if units.len() <= max_chars { (line.into(), false) }
    else { (format!("{}... [truncated]", String::from_utf16_lossy(&units[..max_chars])), true) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn head_preserves_complete_lines() {
        let r = truncate_head("one\ntwo\nthree\n", TruncationOptions { max_lines: 2, max_bytes: 100 });
        assert_eq!(r.content, "one\ntwo"); assert_eq!(r.total_lines, 3); assert_eq!(r.truncated_by.as_deref(), Some("lines"));
    }
    #[test] fn tail_respects_utf8_boundary() {
        let r = truncate_tail("a한글", TruncationOptions { max_lines: 10, max_bytes: 4 });
        assert_eq!(r.content, "글"); assert!(r.last_line_partial);
    }
    #[test] fn empty_and_trailing_newline_counts() {
        assert_eq!(truncate_head("", TruncationOptions::default()).total_lines, 0);
        assert_eq!(truncate_head("a\n", TruncationOptions::default()).total_lines, 1);
        assert_eq!(truncate_head("a\n\n", TruncationOptions::default()).total_lines, 2);
    }
    #[test] fn oversized_first_line_is_not_partial() {
        let r = truncate_head("abcd", TruncationOptions { max_lines: 10, max_bytes: 3 });
        assert!(r.first_line_exceeds_limit); assert!(r.content.is_empty());
    }
}
