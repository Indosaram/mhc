//! Port of `tools/output/render.ts`.

use crate::tools::output::types::{TranscriptEntry, TranscriptMode};

pub const TRANSCRIPT_MAX_CHARS: usize = 30000;

const EMPTY_NOTICE: &str = "(no transcript recorded for this task)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub mode: TranscriptMode,
    pub tail_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTranscript {
    pub text: String,
    pub truncated: bool,
}

/// Render transcript entries to text, then apply tail-line and character-cap trimming. `truncated` is
/// true when either the tail cut lines or the char cap elided content, so task_output can flag it.
pub fn render_transcript(entries: &[TranscriptEntry], options: &RenderOptions) -> RenderedTranscript {
    if entries.is_empty() {
        return RenderedTranscript {
            text: EMPTY_NOTICE.to_string(),
            truncated: false,
        };
    }

    let lines: Vec<String> = entries.iter().map(render_entry).collect();
    let (tail, cut) = match options.mode {
        TranscriptMode::Tail => take_tail(lines, options.tail_lines),
        TranscriptMode::Full => (lines, false),
    };
    let joined = tail.join("\n");
    let (text, elided) = cap_text(&joined);
    RenderedTranscript {
        text,
        truncated: cut || elided,
    }
}

fn render_entry(entry: &TranscriptEntry) -> String {
    match entry {
        TranscriptEntry::Assistant { text } => format!("assistant: {text}"),
        TranscriptEntry::Error { message } => format!("error: {message}"),
        TranscriptEntry::Tool { tool, is_error } => {
            let marker = if *is_error { "tool[error]" } else { "tool" };
            format!("{marker}: {tool}")
        }
    }
}

fn take_tail(lines: Vec<String>, tail_lines: usize) -> (Vec<String>, bool) {
    if tail_lines == 0 || lines.len() <= tail_lines {
        let cut = lines.len() > tail_lines;
        return (lines, cut);
    }
    let start = lines.len() - tail_lines;
    (lines[start..].to_vec(), true)
}

fn marker(elided: usize) -> String {
    format!("\n...[elided {elided} chars]...\n")
}

/// Lengths and slicing follow JS semantics (UTF-16 code units).
fn cap_text(text: &str) -> (String, bool) {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= TRANSCRIPT_MAX_CHARS {
        return (text.to_string(), false);
    }
    let budget = TRANSCRIPT_MAX_CHARS.saturating_sub(marker(units.len()).encode_utf16().count());
    let head_len = budget / 2;
    let tail_len = budget - head_len;
    let elided_count = units.len() - head_len - tail_len;
    let head = String::from_utf16_lossy(&units[..head_len]);
    let tail = String::from_utf16_lossy(&units[units.len() - tail_len..]);
    (format!("{head}{}{tail}", marker(elided_count)), true)
}
