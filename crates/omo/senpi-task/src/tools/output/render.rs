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

    let (kept, cut) = match options.mode {
        TranscriptMode::Tail => take_tail(entries, options.tail_lines),
        TranscriptMode::Full => (entries, false),
    };
    let tail: Vec<String> = kept.iter().map(render_entry).collect();
    let joined = tail.join("\n");
    let (text, elided) = cap_text(joined);
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

// Entries map 1:1 to rendered lines, so the tail is cut before rendering.
fn take_tail(entries: &[TranscriptEntry], tail_lines: usize) -> (&[TranscriptEntry], bool) {
    if tail_lines == 0 || entries.len() <= tail_lines {
        let cut = entries.len() > tail_lines;
        return (entries, cut);
    }
    (&entries[entries.len() - tail_lines..], true)
}

fn marker(elided: usize) -> String {
    format!("\n...[elided {elided} chars]...\n")
}

/// Lengths and slicing follow JS semantics (UTF-16 code units).
fn cap_text(text: String) -> (String, bool) {
    // UTF-16 units never outnumber UTF-8 bytes, so a short string needs no unit buffer or copy.
    if text.len() <= TRANSCRIPT_MAX_CHARS {
        return (text, false);
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= TRANSCRIPT_MAX_CHARS {
        return (text, false);
    }
    let budget = TRANSCRIPT_MAX_CHARS.saturating_sub(marker(units.len()).encode_utf16().count());
    let head_len = budget / 2;
    let tail_len = budget - head_len;
    let elided_count = units.len() - head_len - tail_len;
    let head = String::from_utf16_lossy(&units[..head_len]);
    let tail = String::from_utf16_lossy(&units[units.len() - tail_len..]);
    (format!("{head}{}{tail}", marker(elided_count)), true)
}
