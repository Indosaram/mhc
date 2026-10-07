//! Bounded text for the event stream (latest `kibitzer/events-text.ts`).
//!
//! JS `String.length`/`slice` count UTF-16 code units, so every cap/truncation here operates on
//! UTF-16 units (`encode_utf16`), never Rust `chars()` (Unicode scalar values). This preserves
//! upstream astral behavior: a `.slice` that splits a surrogate pair leaves a dangling high
//! surrogate, which `without_dangling_surrogate` drops exactly as upstream does.

use maho_core::sensitive_output::redact_sensitive_output;
use memory_core::sync::redact::redact_url;

pub const KIBITZER_DIGEST_MAX_CHARS: usize = 1024;
pub const DIGEST_TAIL_FRAGMENTS: usize = 12;
const DIGEST_FRAGMENT_CHARS: usize = 72;

/// JS `String.length`: UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `String.slice(0, n)` then drop a dangling high surrogate (upstream `withoutDanglingSurrogate`).
pub fn utf16_head(text: &str, n: usize) -> String {
    let mut units: Vec<u16> = text.encode_utf16().take(n).collect();
    if let Some(&last) = units.last()
        && (0xd800..=0xdbff).contains(&last)
    {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldState {
    pub count: usize,
    pub first_seq: usize,
    pub last_seq: usize,
    pub first_cursor: usize,
    pub last_cursor: usize,
    pub head: String,
    pub tail: Vec<String>,
}

/// memory-core masks first, then the pinned senpi `core/sensitive-output` patterns
/// (`maho-core::sensitive_output`), matching upstream `redactSensitiveOutput(redactUrl(text))`.
pub fn redact_kibitzer_event_text(text: &str) -> String {
    redact_sensitive_output(&redact_url(text))
}

/// `truncateHead(text, cap)` in JS UTF-16 units; the marker itself displaces units and, for a cap
/// too small for the marker, is a plain cut.
pub fn truncate_head(text: &str, cap: usize) -> String {
    let length = utf16_len(text);
    // Upstream short-input branch: a value at or under the cap is returned unchanged (no marker).
    if length <= cap {
        return text.to_string();
    }
    let marker = format!(" [+{} chars]", length - cap);
    let marker_len = marker.len(); // ASCII: bytes == UTF-16 units
    if cap <= marker_len {
        return utf16_head(text, cap);
    }
    format!("{}{marker}", utf16_head(text, cap - marker_len))
}

/// One folded event as it appears in the digest: kind (and tool), error flag, clipped body.
pub fn fragment_of(kind: &str, tool: Option<&str>, is_error: bool, body: &str) -> String {
    let label = match tool { Some(tool) => format!("{kind}({tool})"), None => kind.to_string() };
    let status = if is_error { " error" } else { "" };
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped = if utf16_len(&collapsed) > DIGEST_FRAGMENT_CHARS {
        format!("{}...", utf16_head(&collapsed, DIGEST_FRAGMENT_CHARS))
    } else {
        collapsed
    };
    format!("{label}{status} \"{clipped}\"")
}

/// Header (count, seq range, cursor range) plus the head fragment, an elision marker, and as many
/// of the newest tail fragments as fit, redacted after composition and hard-capped from the tail so
/// the header - and with it both cursors - always survives.
pub fn digest_line(fold: &FoldState) -> String {
    let header = format!("{} earlier events folded (seq {}-{}, cursor {}..{}): ", fold.count, fold.first_seq, fold.last_seq, fold.first_cursor, fold.last_cursor);
    let budget = KIBITZER_DIGEST_MAX_CHARS.saturating_sub(utf16_len(&header));
    let mut tail = fold.tail.clone();
    let mut elided = fold.count.saturating_sub(1).saturating_sub(tail.len());
    let mut body = join_fragments(&fold.head, elided, &tail);
    while utf16_len(&body) > budget && !tail.is_empty() {
        tail.remove(0);
        elided += 1;
        body = join_fragments(&fold.head, elided, &tail);
    }
    let line = redact_kibitzer_event_text(&format!("{header}{body}")).split(['\r', '\n']).collect::<Vec<_>>().join(" ");
    if utf16_len(&line) <= KIBITZER_DIGEST_MAX_CHARS { line } else { utf16_head(&line, KIBITZER_DIGEST_MAX_CHARS) }
}

fn join_fragments(head: &str, elided: usize, tail: &[String]) -> String {
    let mut parts = vec![head.to_string()];
    if elided > 0 { parts.push(format!("... {elided} more ...")); }
    parts.extend(tail.iter().cloned());
    parts.join(" | ")
}

/// `escapeXml(text)`.
pub fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

