//! StdinBuffer buffers input and emits complete sequences (port of senpi `stdin-buffer.ts`).
//!
//! Stdin chunks can split escape sequences (e.g. SGR mouse `\x1b[<35;20;5m` arriving as
//! `\x1b`, `[<35`, `;20;5m`); the buffer accumulates until a sequence is complete.
//!
//! senpi drives its flush timer with `setTimeout`/`Date.now()`. Here time is an injected
//! [`Clock`]: the owner asks [`StdinBuffer::deadline`] when the pending timer fires and calls
//! [`StdinBuffer::fire_timer`] at or after it. Events are returned instead of emitted.
//!
//! Based on code from OpenTUI (https://github.com/anomalyco/opentui)
//! MIT License - Copyright (c) 2025 opentui

use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

use regex::Regex;

const ESC: &str = "\x1b";
const DEFAULT_SEQUENCE_TIMEOUT_MS: u64 = 50;
const DEFAULT_ESCAPE_TIMEOUT_MS: u64 = 10;
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";
const MOUSE_FRAGMENT_MAX_LEN: usize = 64;
const MOUSE_FRAGMENT_GRACE_MS: u64 = 750;

/// Millisecond clock; tests inject a manual one.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Monotonic wall clock used outside tests.
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinEvent {
    Data(String),
    Paste(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Completeness {
    Complete,
    Incomplete,
    NotEscape,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn is_complete_sequence(data: &str) -> Completeness {
    if !data.starts_with(ESC) {
        return Completeness::NotEscape;
    }
    if data.len() == 1 {
        return Completeness::Incomplete;
    }
    let after_esc = &data[1..];
    if after_esc.starts_with('[') {
        if after_esc.starts_with("[M") {
            // Old-style mouse: ESC [ M + 3 bytes.
            return if utf16_len(data) >= 6 {
                Completeness::Complete
            } else {
                Completeness::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }
    if after_esc.starts_with(']') {
        return is_complete_osc_sequence(data);
    }
    if after_esc.starts_with('P') || after_esc.starts_with('_') {
        return ends_with_st(data);
    }
    if after_esc.starts_with('O') {
        return if utf16_len(after_esc) >= 2 {
            Completeness::Complete
        } else {
            Completeness::Incomplete
        };
    }
    Completeness::Complete
}

static SGR_MOUSE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<[0-9]+;[0-9]+;[0-9]+[Mm]$").expect("valid regex"));

fn is_complete_csi_sequence(data: &str) -> Completeness {
    if utf16_len(data) < 3 {
        return Completeness::Incomplete;
    }
    let payload = &data[2..];
    let Some(last_char) = payload.chars().next_back() else {
        return Completeness::Incomplete;
    };
    if ('\x40'..='\x7e').contains(&last_char) {
        if payload.starts_with('<') {
            if SGR_MOUSE_REGEX.is_match(payload) {
                return Completeness::Complete;
            }
            if last_char == 'M' || last_char == 'm' {
                let inner = &payload[1..payload.len() - 1];
                let parts: Vec<&str> = inner.split(';').collect();
                if parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Completeness::Complete;
                }
            }
            return Completeness::Incomplete;
        }
        return Completeness::Complete;
    }
    Completeness::Incomplete
}

fn is_complete_osc_sequence(data: &str) -> Completeness {
    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        Completeness::Complete
    } else {
        Completeness::Incomplete
    }
}

fn ends_with_st(data: &str) -> Completeness {
    if data.ends_with("\x1b\\") {
        Completeness::Complete
    } else {
        Completeness::Incomplete
    }
}

static UNMODIFIED_KITTY_PRINTABLE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\x1b\[([0-9]+)(?::[0-9]*)?(?::[0-9]+)?u$").expect("valid regex")
});

fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u64> {
    let m = UNMODIFIED_KITTY_PRINTABLE_REGEX.captures(sequence)?;
    let codepoint = m[1].parse::<u64>().unwrap_or(u64::MAX);
    (codepoint >= 32).then_some(codepoint)
}

struct Extracted {
    sequences: Vec<String>,
    remainder: String,
}

fn extract_complete_sequences(buffer: &str) -> Extracted {
    let mut sequences = Vec::new();
    let mut pos = 0;
    while pos < buffer.len() {
        let remaining = &buffer[pos..];
        if remaining.starts_with(ESC) {
            let mut boundaries = remaining
                .char_indices()
                .map(|(i, _)| i)
                .skip(1)
                .chain([remaining.len()]);
            let mut consumed = None;
            for seq_end in boundaries.by_ref() {
                let candidate = &remaining[..seq_end];
                match is_complete_sequence(candidate) {
                    Completeness::Complete => {
                        // WezTerm sends Escape press as raw ESC and its release as CSI-u, so
                        // `\x1b\x1b[27;...u` must split as ESC + a new sequence, not a meta key.
                        if candidate == "\x1b\x1b"
                            && remaining[seq_end..].starts_with(['[', ']', 'O', 'P', '_'])
                        {
                            sequences.push(ESC.to_string());
                            consumed = Some(1);
                            break;
                        }
                        sequences.push(candidate.to_string());
                        consumed = Some(seq_end);
                        break;
                    }
                    Completeness::Incomplete => {}
                    Completeness::NotEscape => {
                        sequences.push(candidate.to_string());
                        consumed = Some(seq_end);
                        break;
                    }
                }
            }
            match consumed {
                Some(n) => pos += n,
                None => {
                    return Extracted {
                        sequences,
                        remainder: remaining.to_string(),
                    };
                }
            }
        } else {
            let ch_len = remaining.chars().next().map_or(1, char::len_utf8);
            sequences.push(remaining[..ch_len].to_string());
            pos += ch_len;
        }
    }
    Extracted {
        sequences,
        remainder: String::new(),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StdinBufferOptions {
    /// Max wait for an incomplete sequence such as CSI or mouse (default 50ms).
    pub timeout: Option<u64>,
    /// Max wait after a lone ESC before treating it as Escape (default 10ms).
    pub escape_timeout: Option<u64>,
}

/// Incremental UTF-8 decoder with Node `StringDecoder` semantics: a truncated trailing
/// character is held back, invalid bytes decode to U+FFFD.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn write(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    out.push_str(valid);
                    rest = &[];
                    break;
                }
                Err(err) => {
                    let (valid, after) = rest.split_at(err.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match err.error_len() {
                        Some(bad) => {
                            out.push('\u{FFFD}');
                            rest = &after[bad..];
                        }
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }
}

/// Input chunk: already-decoded text or raw bytes from stdin.
pub enum StdinInput<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}

impl<'a> From<&'a str> for StdinInput<'a> {
    fn from(value: &'a str) -> Self {
        Self::Text(value)
    }
}

impl<'a> From<&'a [u8]> for StdinInput<'a> {
    fn from(value: &'a [u8]) -> Self {
        Self::Bytes(value)
    }
}

pub struct StdinBuffer {
    buffer: String,
    deadline: Option<u64>,
    deadline_flushes_only: bool,
    timeout_ms: u64,
    escape_timeout_ms: u64,
    mouse_fragment_started_at: Option<u64>,
    discarding_mouse_fragment: bool,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<u64>,
    decoder: Utf8Decoder,
    clock: Arc<dyn Clock>,
}

impl StdinBuffer {
    pub fn new(options: StdinBufferOptions) -> Self {
        Self::with_clock(options, Arc::new(SystemClock::default()))
    }

    pub fn with_clock(options: StdinBufferOptions, clock: Arc<dyn Clock>) -> Self {
        Self {
            buffer: String::new(),
            deadline: None,
            deadline_flushes_only: false,
            timeout_ms: options.timeout.unwrap_or(DEFAULT_SEQUENCE_TIMEOUT_MS),
            escape_timeout_ms: options.escape_timeout.unwrap_or(DEFAULT_ESCAPE_TIMEOUT_MS),
            mouse_fragment_started_at: None,
            discarding_mouse_fragment: false,
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
            decoder: Utf8Decoder::default(),
            clock,
        }
    }

    /// Clock time (ms) at which the pending timer fires, if any.
    pub fn deadline(&self) -> Option<u64> {
        self.deadline
    }

    /// Runs the pending timer callback if its deadline has passed.
    pub fn fire_timer(&mut self) -> Vec<StdinEvent> {
        let Some(deadline) = self.deadline else {
            return Vec::new();
        };
        if self.clock.now_ms() < deadline {
            return Vec::new();
        }
        let flush_only = self.deadline_flushes_only;
        self.deadline = None;
        let flushed = self.flush();
        let mut events = Vec::new();
        if !flush_only {
            for sequence in flushed {
                self.emit_data_sequence(sequence, &mut events);
            }
        }
        events
    }

    fn set_timer(&mut self, ms: u64, flush_only: bool) {
        self.deadline = Some(self.clock.now_ms().saturating_add(ms));
        self.deadline_flushes_only = flush_only;
    }

    pub fn process<'a>(&mut self, data: impl Into<StdinInput<'a>>) -> Vec<StdinEvent> {
        let mut events = Vec::new();
        self.process_into(data.into(), &mut events);
        events
    }

    fn process_into(&mut self, data: StdinInput<'_>, events: &mut Vec<StdinEvent>) {
        self.deadline = None;

        let mut decoded_from_buffer = false;
        let mut s: String = match data {
            StdinInput::Bytes(bytes) => {
                if !self.decoder.has_pending()
                    && bytes.len() == 1
                    && (0x80..0xc2).contains(&bytes[0])
                {
                    // Legacy meta: a high-bit byte is ESC + (byte - 128).
                    let mut s = String::from(ESC);
                    s.push(char::from(bytes[0] - 128));
                    s
                } else {
                    decoded_from_buffer = true;
                    self.decoder.write(bytes)
                }
            }
            StdinInput::Text(text) => text.to_string(),
        };

        if s.is_empty() && self.buffer.is_empty() {
            if !decoded_from_buffer {
                self.emit_data_sequence(String::new(), events);
            }
            return;
        }

        if self.discarding_mouse_fragment {
            let terminator = s.find(|c: char| ('\x40'..='\x7e').contains(&c));
            let escape_index = s.find(ESC);
            match (escape_index, terminator) {
                (Some(e), t) if t.is_none_or(|t| e < t) => s = s[e..].to_string(),
                (_, Some(t)) => s = s[t + 1..].to_string(),
                (_, None) => return,
            }
            self.discarding_mouse_fragment = false;
        }
        self.buffer.push_str(&s);

        if self.paste_mode {
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();
            self.finish_paste_if_complete(events);
            return;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            if start_index > 0 {
                let before = self.buffer[..start_index].to_string();
                for sequence in extract_complete_sequences(&before).sequences {
                    self.emit_data_sequence(sequence, events);
                }
            }
            self.pending_kitty_printable_codepoint = None;
            self.paste_buffer =
                self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.paste_mode = true;
            self.buffer.clear();
            self.finish_paste_if_complete(events);
            return;
        }

        let result = extract_complete_sequences(&self.buffer);
        self.buffer = result.remainder;
        if self.buffer.starts_with("\x1b[<") {
            if self.mouse_fragment_started_at.is_none() {
                self.mouse_fragment_started_at = Some(self.clock.now_ms());
            }
            if utf16_len(&self.buffer) > MOUSE_FRAGMENT_MAX_LEN {
                self.buffer.clear();
                self.discarding_mouse_fragment = true;
            }
        } else {
            self.mouse_fragment_started_at = None;
        }

        for sequence in result.sequences {
            self.emit_data_sequence(sequence, events);
        }

        if !self.buffer.is_empty() {
            let ms = if self.buffer == ESC {
                self.escape_timeout_ms
            } else {
                self.timeout_ms
            };
            self.set_timer(ms, false);
        }
    }

    fn finish_paste_if_complete(&mut self, events: &mut Vec<StdinEvent>) {
        let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) else {
            return;
        };
        let pasted = self.paste_buffer[..end_index].to_string();
        let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
        events.push(StdinEvent::Paste(pasted));
        if !remaining.is_empty() {
            self.process_into(StdinInput::Text(&remaining), events);
        }
    }

    fn emit_data_sequence(&mut self, sequence: String, events: &mut Vec<StdinEvent>) {
        let mut chars = sequence.chars();
        let raw_codepoint = match (chars.next(), chars.next()) {
            (Some(c), None) if c.len_utf16() == 1 => Some(u64::from(u32::from(c))),
            _ => None,
        };
        if raw_codepoint.is_some() && raw_codepoint == self.pending_kitty_printable_codepoint {
            self.pending_kitty_printable_codepoint = None;
            return;
        }
        self.pending_kitty_printable_codepoint =
            parse_unmodified_kitty_printable_codepoint(&sequence);
        events.push(StdinEvent::Data(sequence));
    }

    pub fn flush(&mut self) -> Vec<String> {
        self.deadline = None;
        if self.buffer.is_empty() {
            return Vec::new();
        }
        if self.buffer.starts_with("\x1b[<") {
            let now = self.clock.now_ms();
            let started = self.mouse_fragment_started_at.unwrap_or(now);
            let elapsed = now.saturating_sub(started);
            let remaining = MOUSE_FRAGMENT_GRACE_MS.saturating_sub(elapsed);
            if remaining > 0 && utf16_len(&self.buffer) <= MOUSE_FRAGMENT_MAX_LEN {
                self.set_timer(remaining, true);
            } else {
                self.buffer.clear();
                self.mouse_fragment_started_at = None;
                self.discarding_mouse_fragment = true;
            }
            return Vec::new();
        }
        let sequences = vec![std::mem::take(&mut self.buffer)];
        self.pending_kitty_printable_codepoint = None;
        sequences
    }

    pub fn clear(&mut self) {
        self.deadline = None;
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.mouse_fragment_started_at = None;
        self.discarding_mouse_fragment = false;
        self.pending_kitty_printable_codepoint = None;
        self.decoder = Utf8Decoder::default();
    }

    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    pub fn destroy(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
#[path = "stdin_buffer_tests.rs"]
mod tests;
