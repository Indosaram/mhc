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
pub struct TruncationOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}
impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
/// Borrow the joined split lines beginning at a validated zero-based offset.
pub(crate) fn select_lines(text: &str, start: usize, count: Option<usize>) -> &str {
    let begin = start
        .checked_sub(1)
        .and_then(|n| text.match_indices('\n').nth(n))
        .map_or(0, |(i, _)| i + 1);
    let suffix = &text[begin..];
    match count {
        None => suffix,
        Some(0) => "",
        Some(n) => suffix
            .match_indices('\n')
            .nth(n - 1)
            .map_or(suffix, |(i, _)| &suffix[..i]),
    }
}
fn truncate(content: &str, options: TruncationOptions, tail: bool) -> TruncationResult {
    let mut lines = content.split_terminator('\n');
    let total_lines = lines.clone().count();
    let mut result = TruncationResult {
        content: String::new(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes: content.len(),
        output_lines: 0,
        output_bytes: 0,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    };
    if total_lines <= options.max_lines && content.len() <= options.max_bytes {
        result.content = content.into();
        result.output_lines = total_lines;
        result.output_bytes = content.len();
        return result;
    }
    result.truncated = true;
    result.truncated_by = Some("lines".into());
    if !tail
        && lines
            .clone()
            .next()
            .is_some_and(|line| line.len() > options.max_bytes)
    {
        result.first_line_exceeds_limit = true;
        result.truncated_by = Some("bytes".into());
        return result;
    }
    while result.output_lines < options.max_lines {
        let line = if tail {
            lines.next_back()
        } else {
            lines.next()
        };
        let Some(line) = line else { break };
        let size = line.len() + usize::from(result.output_lines != 0);
        if size > options.max_bytes - result.output_bytes {
            result.truncated_by = Some("bytes".into());
            if tail && result.output_lines == 0 {
                let start = line.ceil_char_boundary(line.len() - options.max_bytes);
                result.output_bytes = line.len() - start;
                result.output_lines = 1;
                result.last_line_partial = true;
            }
            break;
        }
        result.output_bytes += size;
        result.output_lines += 1;
    }
    if result.output_lines == options.max_lines {
        result.truncated_by = Some("lines".into());
    }
    // Joining split_terminator lines preserves interior separators, never the final LF.
    let body = content.strip_suffix('\n').unwrap_or(content);
    result.content = if tail {
        &body[body.len() - result.output_bytes..]
    } else {
        &body[..result.output_bytes]
    }
    .into();
    result
}
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    truncate(content, options, false)
}
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    truncate(content, options, true)
}
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    // PERF-DIVERGENCE(senpi truncate.ts): upstream reads JS string length (already UTF-16); this port skips the UTF-16 Vec when the byte length already fits, since UTF-16 units never exceed UTF-8 bytes. Output is identical.
    if line.len() <= max_chars {
        return (line.into(), false);
    }
    let units: Vec<_> = line.encode_utf16().collect();
    if units.len() <= max_chars {
        (line.into(), false)
    } else {
        (
            format!(
                "{}... [truncated]",
                String::from_utf16_lossy(&units[..max_chars])
            ),
            true,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiddleTruncationResult {
    pub content: String,
    pub truncated: bool,
    pub removed_chars: usize,
    pub total_bytes: usize,
    pub total_lines: usize,
}
pub fn truncate_middle(content: &str, max_bytes: usize) -> MiddleTruncationResult {
    let total_bytes = content.len();
    let total_lines = if content.is_empty() {
        0
    } else {
        content.split_terminator('\n').count()
    };
    if total_bytes <= max_bytes {
        return MiddleTruncationResult {
            content: content.into(),
            truncated: false,
            removed_chars: 0,
            total_bytes,
            total_lines,
        };
    }
    let bytes = content.as_bytes();
    let half = max_bytes / 2;
    let mut head_end = half.min(bytes.len());
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = bytes.len().saturating_sub(max_bytes - half);
    while tail_start < bytes.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let removed_chars = content[head_end..tail_start].chars().count();
    MiddleTruncationResult {
        content: format!(
            "{}…{} chars truncated…{}",
            &content[..head_end],
            removed_chars,
            &content[tail_start..]
        ),
        truncated: true,
        removed_chars,
        total_bytes,
        total_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn head_preserves_complete_lines() {
        let r = truncate_head(
            "one\ntwo\nthree\n",
            TruncationOptions {
                max_lines: 2,
                max_bytes: 100,
            },
        );
        assert_eq!(r.content, "one\ntwo");
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.truncated_by.as_deref(), Some("lines"));
    }
    #[test]
    fn tail_respects_utf8_boundary() {
        let r = truncate_tail(
            "a한글",
            TruncationOptions {
                max_lines: 10,
                max_bytes: 4,
            },
        );
        eprintln!(
            "RP2_OWNER {}",
            serde_json::json!({"tag": "tail", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": r})
        );
        assert_eq!(r.content, "글");
        assert!(r.last_line_partial);
    }
    #[test]
    fn empty_and_trailing_newline_counts() {
        assert_eq!(
            truncate_head("", TruncationOptions::default()).total_lines,
            0
        );
        assert_eq!(
            truncate_head("a\n", TruncationOptions::default()).total_lines,
            1
        );
        assert_eq!(
            truncate_head("a\n\n", TruncationOptions::default()).total_lines,
            2
        );
    }
    #[test]
    fn oversized_first_line_is_not_partial() {
        let r = truncate_head(
            "abcd",
            TruncationOptions {
                max_lines: 10,
                max_bytes: 3,
            },
        );
        assert!(r.first_line_exceeds_limit);
        assert!(r.content.is_empty());
    }
    #[test]
    fn middle_truncation_preserves_utf8_boundaries() {
        let result = truncate_middle("start한글end", 8);
        assert!(result.truncated);
        assert!(result.content.starts_with("sta"));
        assert!(result.content.ends_with("end"));
        assert!(result.content.contains(" chars truncated…"));
    }
}
