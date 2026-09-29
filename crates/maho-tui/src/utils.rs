//! Width, wrapping, truncation and ANSI helpers (port of senpi `utils.ts`).
//!
//! Character classes come from `unicode_tables.rs`, generated from bun's own regex property
//! engine and senpi's `get-east-asian-width` 1.7.0, so widths match senpi exactly; grapheme
//! clusters come from `unicode-segmentation` (UAX #29, Unicode 17 like bun's ICU 78).
//!
//! Positions are UTF-8 byte offsets into `&str` where senpi uses UTF-16 indices.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::process_env;
use crate::unicode_tables::{CC, CF, CJK, CODEPOINT_FLAGS, CS, DI, MARK, MC, RGI_EMOJI, WIDE, WS};

fn flags(c: char) -> u16 {
    let cp = u32::from(c);
    match CODEPOINT_FLAGS.binary_search_by(|&(lo, hi, _)| {
        if hi < cp {
            std::cmp::Ordering::Less
        } else if lo > cp {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => CODEPOINT_FLAGS[i].2,
        Err(_) => 0,
    }
}

fn has(c: char, mask: u16) -> bool {
    flags(c) & mask != 0
}

fn east_asian_width(c: char) -> usize {
    if has(c, WIDE) { 2 } else { 1 }
}

/// Iterates extended grapheme clusters (senpi's shared `Intl.Segmenter` with grapheme granularity).
pub fn graphemes(text: &str) -> impl Iterator<Item = &str> {
    text.graphemes(true)
}

/// A word-granularity segment, mirroring `Intl.SegmentData` for word segmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordSegment<'a> {
    pub segment: &'a str,
    pub index: usize,
    pub is_word_like: bool,
}

/// Word segments (senpi's shared `Intl.Segmenter` with word granularity). ICU's word break
/// rules with dictionary segmentation for CJK/Thai-family scripts, like ICU4C behind
/// `Intl.Segmenter`; `is_word_like` is ICU's non-"none" rule status.
pub fn word_segments(text: &str) -> Vec<WordSegment<'_>> {
    static SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
        LazyLock::new(|| WordSegmenter::new_dictionary(WordBreakInvariantOptions::default()));
    let mut segments = Vec::new();
    let mut breaks = SEGMENTER.segment_str(text).iter_with_word_type();
    let mut start = breaks.next().map_or(0, |(index, _)| index);
    for (end, word_type) in breaks {
        segments.push(WordSegment {
            segment: &text[start..end],
            index: start,
            is_word_like: word_type.is_word_like(),
        });
        start = end;
    }
    segments
}

/// Heuristic pre-filter before the RGI emoji lookup; UTF-16 length > 2 like senpi.
fn could_be_emoji(segment: &str) -> bool {
    let Some(first) = segment.chars().next() else {
        return false;
    };
    let cp = u32::from(first);
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || segment.contains('\u{FE0F}')
        || segment.encode_utf16().count() > 2
}

fn is_rgi_emoji(segment: &str) -> bool {
    RGI_EMOJI.binary_search(&segment).is_ok()
}

const ZERO_WIDTH: u16 = DI | CC | MARK | CS;
const NON_PRINTING: u16 = DI | CC | CF | MARK | CS;

fn is_terminal_spacing_mark(c: char) -> bool {
    let cp = u32::from(c);
    (has(c, MC) && !matches!(cp, 0x1734 | 0x302e | 0x302f))
        || matches!(cp, 0x065f | 0x0f7f | 0x102b | 0x102c | 0x1031 | 0x1033..=0x1035 | 0x1038 | 0x103a..=0x103e)
}

const WIDTH_CACHE_GENERATION_SIZE: usize = 2048;

#[derive(Default)]
struct WidthCache {
    current: HashMap<String, usize>,
    previous: HashMap<String, usize>,
    hits: usize,
    misses: usize,
}

impl WidthCache {
    fn set(&mut self, s: &str, width: usize) {
        self.current.insert(s.to_string(), width);
        if self.current.len() >= WIDTH_CACHE_GENERATION_SIZE {
            self.previous = std::mem::take(&mut self.current);
        }
    }
}

thread_local! {
    static WIDTH_CACHE: RefCell<WidthCache> = RefCell::new(WidthCache::default());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidthCacheStats {
    pub hits: usize,
    pub misses: usize,
    pub current_size: usize,
    pub previous_size: usize,
    pub total_retained: usize,
}

/// Width-cache counters, exposed only with `PI_TUI_TEST_SEAMS=1` like senpi.
pub fn __width_cache_stats() -> Option<WidthCacheStats> {
    if process_env::var("PI_TUI_TEST_SEAMS").as_deref() != Some("1") {
        return None;
    }
    Some(WIDTH_CACHE.with(|c| {
        let c = c.borrow();
        WidthCacheStats {
            hits: c.hits,
            misses: c.misses,
            current_size: c.current.len(),
            previous_size: c.previous.len(),
            total_retained: c.current.len() + c.previous.len(),
        }
    }))
}

/// `cjkBreakRegex.test(segment)`: any Han/Hiragana/Katakana/Hangul/Bopomofo code point.
pub fn is_cjk_break(segment: &str) -> bool {
    segment.chars().any(|c| has(c, CJK))
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// JS `/\s/.test(s)`: true when any character is whitespace.
pub fn is_whitespace_char(s: &str) -> bool {
    s.chars().any(|c| has(c, WS))
}

fn trim_end_js(s: &str) -> &str {
    s.trim_end_matches(|c| has(c, WS))
}

fn is_blank_js(s: &str) -> bool {
    s.chars().all(|c| has(c, WS))
}

/// Terminal width of one grapheme cluster.
pub(crate) fn grapheme_width(segment: &str) -> usize {
    if segment == "\t" {
        return 3;
    }
    if !segment.is_empty() && segment.chars().all(is_terminal_spacing_mark) {
        return segment.chars().count();
    }
    if !segment.is_empty() && segment.chars().all(|c| has(c, ZERO_WIDTH)) {
        return 0;
    }
    if could_be_emoji(segment) && is_rgi_emoji(segment) {
        return 2;
    }

    let base = segment.trim_start_matches(|c| has(c, NON_PRINTING));
    let mut chars = base.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    // Regional indicators render full-width even when isolated mid-stream.
    if ('\u{1f1e6}'..='\u{1f1ff}').contains(&first) {
        return 2;
    }

    let mut width = east_asian_width(first);
    let mut follows_mark = false;
    for c in chars {
        if is_terminal_spacing_mark(c) {
            width += 1;
            follows_mark = false;
        } else if has(c, MARK) {
            follows_mark = true;
        } else if !has(c, NON_PRINTING) {
            let cp = u32::from(c);
            if follows_mark || (0xff00..=0xffef).contains(&cp) {
                width += east_asian_width(c);
            } else if cp == 0x0e33 || cp == 0x0eb3 {
                width += 1;
            }
            follows_mark = false;
        }
    }
    width
}

/// Visible width of a string in terminal columns (tabs are 3, escape sequences are 0).
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if is_printable_ascii(s) {
        return s.len();
    }
    let cached = WIDTH_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(&w) = c.current.get(s) {
            c.hits += 1;
            return Some(w);
        }
        if let Some(&w) = c.previous.get(s) {
            c.hits += 1;
            c.set(s, w);
            return Some(w);
        }
        c.misses += 1;
        None
    });
    if let Some(w) = cached {
        return w;
    }

    let mut clean = if s.contains('\t') {
        s.replace('\t', "   ")
    } else {
        s.to_string()
    };
    if clean.contains('\x1b') {
        clean = strip_terminal_sequences(&clean);
    }
    let width = graphemes(&clean).map(grapheme_width).sum();
    WIDTH_CACHE.with(|c| c.borrow_mut().set(s, width));
    width
}

/// Remove ANSI, OSC, DCS and APC control sequences while preserving visible text.
pub fn strip_terminal_sequences(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some(code) = extract_ansi_code(s, i) {
            i += code.len();
            continue;
        }
        let ch = next_char_len(s, i);
        result.push_str(&s[i..i + ch]);
        i += ch;
    }
    result
}

fn next_char_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(1, char::len_utf8)
}

/// End of the plain-text run starting at `i` (next escape sequence or end of string).
fn text_run_end(s: &str, i: usize) -> usize {
    let mut end = i;
    while end < s.len() && extract_ansi_code(s, end).is_none() {
        end += next_char_len(s, end);
    }
    end
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphemeCellRange {
    pub start: usize,
    pub end: usize,
}

/// Terminal-cell range occupied by the grapheme at a visible column.
pub fn get_grapheme_cell_range(line: &str, column: usize) -> Option<GraphemeCellRange> {
    let mut current_col = 0;
    let mut i = 0;
    while i < line.len() {
        if let Some(code) = extract_ansi_code(line, i) {
            i += code.len();
            continue;
        }
        let text_end = text_run_end(line, i);
        for segment in graphemes(&line[i..text_end]) {
            let width = grapheme_width(segment);
            if width > 0 && column >= current_col && column < current_col + width {
                return Some(GraphemeCellRange {
                    start: current_col,
                    end: current_col + width,
                });
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

static OSC8_LINK_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\x1b\]8;[^;]*;([^\x07\x1b]*)(?:\x07|\x1b\\)$").expect("valid regex")
});

/// OSC 8 hyperlink covering a visible terminal column.
pub fn get_osc8_link_at_column(line: &str, column: usize) -> Option<String> {
    let mut active_url: Option<String> = None;
    let mut current_col = 0;
    let mut i = 0;
    while i < line.len() {
        if let Some(code) = extract_ansi_code(line, i) {
            if let Some(m) = OSC8_LINK_REGEX.captures(code) {
                active_url = Some(m[1].to_string()).filter(|u| !u.is_empty());
            }
            i += code.len();
            continue;
        }
        let text_end = text_run_end(line, i);
        for segment in graphemes(&line[i..text_end]) {
            let width = grapheme_width(segment);
            if column >= current_col && column < current_col + width {
                return active_url;
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

/// Normalize text for terminal output: decompose Thai/Lao AM vowels and expand visible tabs
/// (tabs inside terminal string sequences stay untouched).
pub fn normalize_terminal_output(s: &str) -> String {
    let normalized = if s.contains(['\u{0e33}', '\u{0eb3}']) {
        s.replace('\u{0e33}', "\u{0e4d}\u{0e32}")
            .replace('\u{0eb3}', "\u{0ecd}\u{0eb2}")
    } else {
        s.to_string()
    };
    if !normalized.contains('\t') {
        return normalized;
    }
    let mut result = String::with_capacity(normalized.len() + 8);
    let mut i = 0;
    while i < normalized.len() {
        if let Some(code) = extract_ansi_code(&normalized, i) {
            result.push_str(code);
            i += code.len();
            continue;
        }
        let ch = next_char_len(&normalized, i);
        let piece = &normalized[i..i + ch];
        result.push_str(if piece == "\t" { "   " } else { piece });
        i += ch;
    }
    result
}

static ADJACENT_SGR_RUN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\x1b\[[0-9;]*m){2,}").expect("valid regex"));
static SGR_IN_RUN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[([0-9;]*)m").expect("valid regex"));

/// Merge runs of adjacent SGR sequences into one.
pub fn coalesce_adjacent_sgr(line: &str) -> String {
    if !line.contains("\x1b[") {
        return line.to_string();
    }
    ADJACENT_SGR_RUN_REGEX
        .replace_all(line, |caps: &regex::Captures<'_>| {
            let params: Vec<&str> = SGR_IN_RUN_REGEX
                .captures_iter(&caps[0])
                .map(|m| {
                    let p = m.get(1).map_or("", |g| g.as_str());
                    if p.is_empty() { "0" } else { p }
                })
                .collect();
            format!("\x1b[{}m", params.join(";"))
        })
        .into_owned()
}

/// Extract the escape sequence starting at byte `pos` (CSI ending in m/G/K/H/J, OSC, DCS, APC).
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return None;
    }
    let next = bytes.get(pos + 1).copied();
    match next {
        Some(b'[') => {
            let mut j = pos + 2;
            while j < bytes.len() && !matches!(bytes[j], b'm' | b'G' | b'K' | b'H' | b'J') {
                j += 1;
            }
            (j < bytes.len()).then(|| &s[pos..=j])
        }
        Some(b']') => {
            let mut j = pos + 2;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    return Some(&s[pos..=j]);
                }
                if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                    return Some(&s[pos..j + 2]);
                }
                j += 1;
            }
            None
        }
        Some(b'P') => {
            // tmux passthrough doubles every ESC in its payload; skip doubled pairs.
            let mut j = pos + 2;
            while j < bytes.len() {
                if bytes[j] == 0x1b {
                    if bytes.get(j + 1) == Some(&b'\\') {
                        return Some(&s[pos..j + 2]);
                    }
                    if bytes.get(j + 1) == Some(&0x1b) {
                        j += 2;
                        continue;
                    }
                }
                j += 1;
            }
            None
        }
        Some(b'_') => {
            let mut j = pos + 2;
            while j < bytes.len() {
                if bytes[j] == 0x07 {
                    return Some(&s[pos..=j]);
                }
                if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                    return Some(&s[pos..j + 2]);
                }
                j += 1;
            }
            None
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Osc8Terminator {
    Bel,
    St,
}

impl Osc8Terminator {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bel => "\x07",
            Self::St => "\x1b\\",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveHyperlink {
    params: String,
    url: String,
    terminator: Osc8Terminator,
}

/// `Some(Some(link))` opens, `Some(None)` closes, `None` means not an OSC 8 sequence.
fn parse_osc8_hyperlink(code: &str) -> Option<Option<ActiveHyperlink>> {
    let rest = code.strip_prefix("\x1b]8;")?;
    let terminator = if code.ends_with('\x07') {
        Osc8Terminator::Bel
    } else {
        Osc8Terminator::St
    };
    let cut = match terminator {
        Osc8Terminator::Bel => 1,
        Osc8Terminator::St => 2,
    };
    let body = rest.get(..rest.len().saturating_sub(cut)).unwrap_or("");
    let sep = body.find(';')?;
    let params = &body[..sep];
    let url = &body[sep + 1..];
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(ActiveHyperlink {
        params: params.to_string(),
        url: url.to_string(),
        terminator,
    }))
}

fn format_osc8_hyperlink(h: &ActiveHyperlink) -> String {
    format!("\x1b]8;{};{}{}", h.params, h.url, h.terminator.as_str())
}

fn format_osc8_close(terminator: Osc8Terminator) -> String {
    format!("\x1b]8;;{}", terminator.as_str())
}

fn get_active_osc8_close(prefix: &str) -> String {
    if !prefix.contains("\x1b]8;") {
        return String::new();
    }
    let mut active: Option<ActiveHyperlink> = None;
    let mut i = 0;
    while i < prefix.len() {
        if let Some(code) = extract_ansi_code(prefix, i) {
            if let Some(h) = parse_osc8_hyperlink(code) {
                active = h;
            }
            i += code.len();
        } else {
            i += next_char_len(prefix, i);
        }
    }
    active
        .map(|h| format_osc8_close(h.terminator))
        .unwrap_or_default()
}

static SGR_PARAMS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[([0-9;]*)m").expect("valid regex"));

/// Tracks active SGR attributes and OSC 8 links to carry styling across line breaks.
#[derive(Debug, Clone, Default)]
pub(crate) struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

fn parse_code(part: &str) -> Option<i64> {
    if part.is_empty() {
        None
    } else {
        Some(part.parse::<i64>().unwrap_or(i64::MAX))
    }
}

impl AnsiCodeTracker {
    pub(crate) fn process(&mut self, code: &str) {
        // Keep the link's original terminator: some terminals only make BEL links clickable.
        if let Some(h) = parse_osc8_hyperlink(code) {
            self.active_hyperlink = h;
            return;
        }
        if !code.ends_with('m') {
            return;
        }
        let Some(m) = SGR_PARAMS_REGEX.captures(code) else {
            return;
        };
        let params = m.get(1).map_or("", |g| g.as_str());
        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }
        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let code = parse_code(parts[i]);
            if matches!(code, Some(38 | 48)) {
                if parts.get(i + 1) == Some(&"5") && i + 2 < parts.len() {
                    let color = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    self.set_color(code == Some(38), color);
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && i + 4 < parts.len() {
                    let color = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    self.set_color(code == Some(38), color);
                    i += 5;
                    continue;
                }
            }
            match code {
                Some(0) => self.reset(),
                Some(1) => self.bold = true,
                Some(2) => self.dim = true,
                Some(3) => self.italic = true,
                Some(4) => self.underline = true,
                Some(5) => self.blink = true,
                Some(7) => self.inverse = true,
                Some(8) => self.hidden = true,
                Some(9) => self.strikethrough = true,
                Some(21) => self.bold = false,
                Some(22) => {
                    self.bold = false;
                    self.dim = false;
                }
                Some(23) => self.italic = false,
                Some(24) => self.underline = false,
                Some(25) => self.blink = false,
                Some(27) => self.inverse = false,
                Some(28) => self.hidden = false,
                Some(29) => self.strikethrough = false,
                Some(39) => self.fg_color = None,
                Some(49) => self.bg_color = None,
                Some(c) if (30..=37).contains(&c) || (90..=97).contains(&c) => {
                    self.fg_color = Some(c.to_string())
                }
                Some(c) if (40..=47).contains(&c) || (100..=107).contains(&c) => {
                    self.bg_color = Some(c.to_string())
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_color(&mut self, foreground: bool, color: String) {
        if foreground {
            self.fg_color = Some(color);
        } else {
            self.bg_color = Some(color);
        }
    }

    /// SGR reset does not affect OSC 8 hyperlink state.
    fn reset(&mut self) {
        let link = self.active_hyperlink.take();
        *self = Self {
            active_hyperlink: link,
            ..Self::default()
        };
    }

    pub(crate) fn get_active_codes(&self) -> String {
        let mut codes: Vec<&str> = Vec::new();
        for (on, code) in [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.blink, "5"),
            (self.inverse, "7"),
            (self.hidden, "8"),
            (self.strikethrough, "9"),
        ] {
            if on {
                codes.push(code);
            }
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg);
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg);
        }
        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some(h) = &self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(h));
        }
        result
    }

    fn get_active_background_code(&self) -> String {
        self.bg_color
            .as_ref()
            .map(|bg| format!("\x1b[{bg}m"))
            .unwrap_or_default()
    }

    fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    /// Codes to close at line end: underline (to avoid bleeding into padding) and an open
    /// OSC 8 link (re-opened on the next line by `get_active_codes`).
    fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\x1b[24m");
        }
        if let Some(h) = &self.active_hyperlink {
            result.push_str(&format_osc8_close(h.terminator));
        }
        result
    }
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut i = 0;
    while i < text.len() {
        if let Some(code) = extract_ansi_code(text, i) {
            tracker.process(code);
            i += code.len();
        } else {
            i += next_char_len(text, i);
        }
    }
}

/// Background color active at the end of an ANSI-styled string.
pub fn get_active_background_ansi(text: &str) -> String {
    let mut tracker = AnsiCodeTracker::default();
    update_tracker_from_text(text, &mut tracker);
    tracker.get_active_background_code()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Space,
    Word,
}

/// Split text into space/word tokens (CJK graphemes are their own tokens), with pending ANSI
/// codes attached to the next visible content.
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut current_kind: Option<TokenKind> = None;
    let mut i = 0;

    while i < text.len() {
        if let Some(code) = extract_ansi_code(text, i) {
            pending_ansi.push_str(code);
            i += code.len();
            continue;
        }
        let end = text_run_end(text, i);
        for segment in graphemes(&text[i..end]) {
            let is_space = segment == " ";
            if !is_space && is_cjk_break(segment) {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                current_kind = None;
                tokens.push(std::mem::take(&mut pending_ansi) + segment);
                continue;
            }
            let kind = if is_space {
                TokenKind::Space
            } else {
                TokenKind::Word
            };
            if !current.is_empty() && current_kind != Some(kind) {
                tokens.push(std::mem::take(&mut current));
            }
            if !pending_ansi.is_empty() {
                current.push_str(&std::mem::take(&mut pending_ansi));
            }
            current_kind = Some(kind);
            current.push_str(segment);
        }
        i = end;
    }

    if !pending_ansi.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending_ansi);
        } else if let Some(last) = tokens.last_mut() {
            last.push_str(&pending_ansi);
        } else {
            current = pending_ansi;
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

static LINE_BREAK_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\r\n|\r|\n").expect("valid regex"));

/// Word-wrap text preserving ANSI codes across line breaks. Lines are not padded.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::default();
    for input_line in LINE_BREAK_REGEX.split(text) {
        let prefix = if result.is_empty() {
            String::new()
        } else {
            tracker.get_active_codes()
        };
        result.extend(wrap_single_line(&(prefix + input_line), width));
        update_tracker_from_text(input_line, &mut tracker);
    }
    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::default();
    let tokens = split_into_tokens_with_ansi(line);
    let mut current_line = String::new();
    let mut current_visible = 0;

    for token in &tokens {
        let token_visible = visible_width(token);
        let is_whitespace = is_blank_js(token);

        if token_visible > width && !is_whitespace {
            if !current_line.is_empty() {
                current_line.push_str(&tracker.get_line_end_reset());
                wrapped.push(std::mem::take(&mut current_line));
            }
            let mut broken = break_long_word(token, width, &mut tracker);
            let last = broken.pop().unwrap_or_default();
            wrapped.extend(broken);
            current_visible = visible_width(&last);
            current_line = last;
            continue;
        }

        if current_visible + token_visible > width && current_visible > 0 {
            let mut line_to_wrap = trim_end_js(&current_line).to_string();
            line_to_wrap.push_str(&tracker.get_line_end_reset());
            wrapped.push(line_to_wrap);
            if is_whitespace {
                current_line = tracker.get_active_codes();
                current_visible = 0;
            } else {
                current_line = tracker.get_active_codes() + token;
                current_visible = token_visible;
            }
        } else {
            current_line.push_str(token);
            current_visible += token_visible;
        }
        update_tracker_from_text(token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }
    if wrapped.is_empty() {
        return vec![String::new()];
    }
    wrapped.iter().map(|l| trim_end_js(l).to_string()).collect()
}

/// `PUNCTUATION_REGEX` membership for one character.
pub fn is_punctuation_char(c: char) -> bool {
    "(){}[]<>.,;:'\"!?+-=*/\\|&%^$#@~`".contains(c)
}

enum WordPart<'a> {
    Ansi(&'a str),
    Grapheme(&'a str),
}

fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width = 0;

    let mut parts = Vec::new();
    let mut i = 0;
    while i < word.len() {
        if let Some(code) = extract_ansi_code(word, i) {
            parts.push(WordPart::Ansi(code));
            i += code.len();
        } else {
            let end = text_run_end(word, i);
            parts.extend(graphemes(&word[i..end]).map(WordPart::Grapheme));
            i = end;
        }
    }

    for part in parts {
        let grapheme = match part {
            WordPart::Ansi(code) => {
                current_line.push_str(code);
                tracker.process(code);
                continue;
            }
            WordPart::Grapheme(g) => g,
        };
        if grapheme.is_empty() {
            continue;
        }
        let gw = visible_width(grapheme);
        if current_width + gw > width {
            current_line.push_str(&tracker.get_line_end_reset());
            lines.push(std::mem::take(&mut current_line));
            current_line = tracker.get_active_codes();
            current_width = 0;
        }
        current_line.push_str(grapheme);
        current_width += gw;
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn sgr_leaves_default_background(params: &str) -> bool {
    if params.is_empty() {
        return true;
    }
    let parts: Vec<&str> = params.split(';').collect();
    let mut background_is_default = false;
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index];
        let code = if part.is_empty() {
            0
        } else {
            part.parse::<i64>().unwrap_or(i64::MAX)
        };
        if code == 0 || code == 49 {
            background_is_default = true;
            index += 1;
            continue;
        }
        if (code == 38 || code == 48)
            && parts.get(index + 1) == Some(&"5")
            && index + 2 < parts.len()
        {
            if code == 48 {
                background_is_default = false;
            }
            index += 3;
            continue;
        }
        if (code == 38 || code == 48)
            && parts.get(index + 1) == Some(&"2")
            && index + 4 < parts.len()
        {
            if code == 48 {
                background_is_default = false;
            }
            index += 5;
            continue;
        }
        if (40..=47).contains(&code) || (100..=107).contains(&code) {
            background_is_default = false;
        }
        index += 1;
    }
    background_is_default
}

/// Apply a background color to a line, padding it to `width`.
pub fn apply_background_to_line(
    line: &str,
    width: usize,
    bg_fn: &dyn Fn(&str) -> String,
) -> String {
    let visible_len = visible_width(line);
    let padding = " ".repeat(width.saturating_sub(visible_len));
    let with_padding = format!("{line}{padding}");
    let marker = "\x1fpi-bg-marker\x1f";
    let wrapped_marker = bg_fn(marker);
    let Some(marker_index) = wrapped_marker.find(marker) else {
        return bg_fn(&with_padding);
    };
    let bg_start = &wrapped_marker[..marker_index];
    let bg_end = &wrapped_marker[marker_index + marker.len()..];
    let restored_line = SGR_PARAMS_REGEX.replace_all(line, |caps: &regex::Captures<'_>| {
        let sequence = &caps[0];
        let params = caps.get(1).map_or("", |g| g.as_str());
        if sgr_leaves_default_background(params) {
            format!("{sequence}{bg_start}")
        } else {
            sequence.to_string()
        }
    });
    let mut tracker = AnsiCodeTracker::default();
    update_tracker_from_text(line, &mut tracker);
    let restored = if !padding.is_empty() && tracker.has_active_codes() {
        format!(
            "{restored_line}\x1b[0m{}{bg_start}{padding}",
            tracker.get_line_end_reset()
        )
    } else {
        format!("{restored_line}{padding}")
    };
    format!("{bg_start}{restored}{bg_end}")
}

struct Fragment {
    text: String,
    width: usize,
}

fn truncate_fragment_to_width(text: &str, max_width: usize) -> Fragment {
    if max_width == 0 || text.is_empty() {
        return Fragment {
            text: String::new(),
            width: 0,
        };
    }
    if is_printable_ascii(text) {
        let clipped = &text[..text.len().min(max_width)];
        return Fragment {
            text: clipped.to_string(),
            width: clipped.len(),
        };
    }
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');
    let mut result = String::new();
    let mut width = 0;
    if !has_ansi && !has_tabs {
        for segment in graphemes(text) {
            let w = grapheme_width(segment);
            if width + w > max_width {
                break;
            }
            result.push_str(segment);
            width += w;
        }
        return Fragment {
            text: result,
            width,
        };
    }

    let mut i = 0;
    let mut pending_ansi = String::new();
    while i < text.len() {
        if let Some(code) = extract_ansi_code(text, i) {
            pending_ansi.push_str(code);
            i += code.len();
            continue;
        }
        if text.as_bytes()[i] == b'\t' {
            if width + 3 > max_width {
                break;
            }
            result.push_str(&std::mem::take(&mut pending_ansi));
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }
        let end = tab_or_ansi_run_end(text, i);
        for segment in graphemes(&text[i..end]) {
            let w = grapheme_width(segment);
            if width + w > max_width {
                return Fragment {
                    text: result,
                    width,
                };
            }
            result.push_str(&std::mem::take(&mut pending_ansi));
            result.push_str(segment);
            width += w;
        }
        i = end;
    }
    Fragment {
        text: result,
        width,
    }
}

fn tab_or_ansi_run_end(text: &str, i: usize) -> usize {
    let mut end = i;
    while end < text.len()
        && text.as_bytes()[end] != b'\t'
        && extract_ansi_code(text, end).is_none()
    {
        end += next_char_len(text, end);
    }
    end
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    let reset = "\x1b[0m";
    let hyperlink_close = get_active_osc8_close(prefix);
    let visible = prefix_width + ellipsis_width;
    let result = if ellipsis.is_empty() {
        format!("{prefix}{hyperlink_close}{reset}")
    } else {
        format!("{prefix}{hyperlink_close}{reset}{ellipsis}{reset}")
    };
    if pad {
        result + &" ".repeat(max_width.saturating_sub(visible))
    } else {
        result
    }
}

/// Truncate text to `max_width` columns, appending `ellipsis` when cut; `pad` fills to width.
pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad {
            " ".repeat(max_width)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad {
                format!("{text}{}", " ".repeat(max_width - text_width))
            } else {
                text.to_string()
            };
        }
        let clipped = truncate_fragment_to_width(ellipsis, max_width);
        if clipped.width == 0 {
            return if pad {
                " ".repeat(max_width)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped.text, clipped.width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return if pad {
                format!("{text}{}", " ".repeat(max_width - text.len()))
            } else {
                text.to_string()
            };
        }
        let target = max_width - ellipsis_width;
        return finalize_truncated_result(
            &text[..target],
            target,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0;
    let mut kept_width = 0;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let exhausted_input;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    if !has_ansi && !has_tabs {
        for segment in graphemes(text) {
            let w = grapheme_width(segment);
            if keep_contiguous_prefix && kept_width + w <= target_width {
                result.push_str(segment);
                kept_width += w;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += w;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
        exhausted_input = !overflowed;
    } else {
        let mut i = 0;
        'outer: while i < text.len() {
            if let Some(code) = extract_ansi_code(text, i) {
                pending_ansi.push_str(code);
                i += code.len();
                continue;
            }
            if text.as_bytes()[i] == b'\t' {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    result.push_str(&std::mem::take(&mut pending_ansi));
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }
            let end = tab_or_ansi_run_end(text, i);
            for segment in graphemes(&text[i..end]) {
                let w = grapheme_width(segment);
                if keep_contiguous_prefix && kept_width + w <= target_width {
                    result.push_str(&std::mem::take(&mut pending_ansi));
                    result.push_str(segment);
                    kept_width += w;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += w;
                if visible_so_far > max_width {
                    overflowed = true;
                    break 'outer;
                }
            }
            i = end;
        }
        exhausted_input = i >= text.len();
    }

    if !overflowed && exhausted_input {
        return if pad {
            format!(
                "{text}{}",
                " ".repeat(max_width.saturating_sub(visible_so_far))
            )
        } else {
            text.to_string()
        };
    }
    finalize_truncated_result(
        &result,
        kept_width,
        ellipsis,
        ellipsis_width,
        max_width,
        pad,
    )
}

/// Extract a range of visible columns from a line. With `strict`, wide characters that would
/// extend past the range are excluded.
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).0
}

/// Like [`slice_by_column`] but also returns the visible width of the result.
pub fn slice_with_width(
    line: &str,
    start_col: usize,
    length: usize,
    strict: bool,
) -> (String, usize) {
    if length == 0 {
        return (String::new(), 0);
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0;
    let mut current_col = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();

    while i < line.len() {
        if let Some(code) = extract_ansi_code(line, i) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(code);
            } else if current_col < start_col {
                pending_ansi.push_str(code);
            }
            i += code.len();
            continue;
        }
        let text_end = text_run_end(line, i);
        for segment in graphemes(&line[i..text_end]) {
            let w = grapheme_width(segment);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                result.push_str(&std::mem::take(&mut pending_ansi));
                result.push_str(segment);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                break;
            }
        }
        i = text_end;
        if current_col >= end_col {
            break;
        }
    }
    (result, result_width)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Segments {
    pub before: String,
    pub before_width: usize,
    pub after: String,
    pub after_width: usize,
}

/// Extract "before" and "after" segments around an overlay region in one pass; "after"
/// inherits styling active before the overlay.
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> Segments {
    let mut out = Segments::default();
    let mut current_col = 0;
    let mut i = 0;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let mut tracker = AnsiCodeTracker::default();
    let done = |col: usize| {
        if after_len == 0 {
            col >= before_end
        } else {
            col >= after_end
        }
    };

    while i < line.len() {
        if let Some(code) = extract_ansi_code(line, i) {
            tracker.process(code);
            if current_col < before_end {
                pending_ansi_before.push_str(code);
            } else if current_col >= after_start && current_col < after_end && after_started {
                out.after.push_str(code);
            }
            i += code.len();
            continue;
        }
        let text_end = text_run_end(line, i);
        for segment in graphemes(&line[i..text_end]) {
            let w = grapheme_width(segment);
            if current_col < before_end && current_col + w <= before_end {
                out.before
                    .push_str(&std::mem::take(&mut pending_ansi_before));
                out.before.push_str(segment);
                out.before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        out.after.push_str(&tracker.get_active_codes());
                        after_started = true;
                    }
                    out.after.push_str(segment);
                    out.after_width += w;
                }
            }
            current_col += w;
            if done(current_col) {
                break;
            }
        }
        i = text_end;
        if done(current_col) {
            break;
        }
    }
    out
}

#[cfg(test)]
#[path = "utils_tests.rs"]
mod tests;
