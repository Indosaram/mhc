//! Port of senpi packages/ai/src/utils/secret-lexer.ts.
//!
//! The lexical layer of the credential redactor (senpi#4283): character classes, percent escapes
//! and HTML entities read as what they encode, the token and delimiter units the one-pass scanner
//! alternates between, and the forward scans that find where a quoted or bare value ends. Every
//! function reads forward from the index it is given; none searches back or uses a regular
//! expression. Text is addressed in UTF-16 code units, as the TS `charCodeAt` reads it.

/// A code unit, or -1 past the end (the TS `NaN`, which every class check rejects).
pub type Code = i32;

#[cfg(test)]
thread_local! {
    pub(crate) static READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `text.charCodeAt(i)`.
#[inline]
pub fn code_at(text: &[u16], i: usize) -> Code {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    text.get(i).map_or(-1, |&c| Code::from(c))
}

const HTML_ENTITIES: &[(&str, Code)] = &[
    ("&quot;", 34),
    ("&#34;", 34),
    ("&#034;", 34),
    ("&#x22;", 34),
    ("&#39;", 39),
    ("&#039;", 39),
    ("&#x27;", 39),
    ("&lt;", 60),
    ("&gt;", 62),
    ("&amp;", 38),
];

const MAX_SEPARATOR_LENGTH: usize = 32;
const MAX_SCHEME_GAP: usize = 8;
const PERCENT_WINDOW: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unit {
    pub character: Code,
    pub length: usize,
    /// `refusal: "percent-window"`.
    pub refusal: bool,
}

impl Unit {
    fn new(character: Code, length: usize) -> Self {
        Self {
            character,
            length,
            refusal: false,
        }
    }
}

pub fn is_digit(c: Code) -> bool {
    (48..=57).contains(&c)
}
pub fn is_upper(c: Code) -> bool {
    (65..=90).contains(&c)
}
pub fn is_lower(c: Code) -> bool {
    (97..=122).contains(&c)
}
pub fn is_alnum(c: Code) -> bool {
    is_digit(c) || is_upper(c) || is_lower(c)
}
pub fn is_base64_url(c: Code) -> bool {
    is_alnum(c) || c == 95 || c == 45
}
pub fn is_name_char(c: Code) -> bool {
    is_base64_url(c) || c == 46
}
fn is_token_char(c: Code) -> bool {
    is_name_char(c) || c == 43 || c == 47 || c == 126
}
fn is_space(c: Code) -> bool {
    c == 32 || c == 9 || c == 10 || c == 13
}
fn is_quote(c: Code) -> bool {
    c == 34 || c == 39 || c == 96 || c == 92
}
fn is_opening_quote(c: Code) -> bool {
    c == 34 || c == 39 || c == 96
}
fn is_typographic_quote(c: Code) -> bool {
    c == 0x201c || c == 0x201d || c == 0x2018 || c == 0x2019
}

fn hex_value(c: Code) -> Code {
    if is_digit(c) {
        c - 48
    } else if (65..=70).contains(&c) {
        c - 55
    } else if (97..=102).contains(&c) {
        c - 87
    } else {
        -1
    }
}

/// Whether the ASCII `literal` is at `i`, ending at or before `limit`.
pub fn matches_at(text: &[u16], i: usize, literal: &str, limit: usize) -> bool {
    if i + literal.len() > limit {
        return false;
    }
    literal
        .bytes()
        .enumerate()
        .all(|(k, byte)| code_at(text, i + k) == Code::from(byte))
}

/// The length of the token unit at i (a value character, or a percent escape of one), or 0 at a
/// delimiter.
pub fn token_unit_length(text: &[u16], i: usize) -> usize {
    let c = code_at(text, i);
    if is_token_char(c) {
        return 1;
    }
    if c != 37 {
        return 0;
    }
    let unit = read_unit(text, i);
    if unit.refusal {
        return 0;
    }
    if is_token_char(unit.character) || unit.length == 1 {
        unit.length
    } else {
        0
    }
}

/// Repeated full-window percent decoding to a fixpoint, keeping each character's original source
/// end. A pass shrinks the 64-character window by at least two, so both work and storage here are
/// bounded. Malformed escapes are literal, but an unfinished escape at the window edge is not a
/// proven boundary.
fn percent_window(text: &[u16], start: usize, prefix_end: usize) -> Unit {
    let limit = text.len().min(prefix_end + PERCENT_WINDOW - 1);
    let mut needs_decoding = false;
    let mut contiguous = true;
    let mut previous: Code = 37;
    for p in prefix_end..limit {
        let c = code_at(text, p);
        let hex = hex_value(c) >= 0;
        if previous == 37 && hex {
            needs_decoding = true;
            break;
        }
        if c != 37 && !hex {
            contiguous = false;
        }
        previous = c;
    }
    // Without a possible escape no pass can change the window, so do not allocate its fixpoint buffers.
    if !needs_decoding {
        if contiguous && limit < text.len() {
            return Unit {
                character: 37,
                length: limit - start,
                refusal: true,
            };
        }
        return Unit::new(37, prefix_end - start);
    }
    let mut characters: Vec<Code> = vec![37];
    let mut ends: Vec<usize> = vec![prefix_end];
    for p in prefix_end..limit {
        characters.push(code_at(text, p));
        ends.push(p + 1);
    }
    loop {
        let mut changed = false;
        let mut written = 0;
        let mut p = 0;
        while p < characters.len() {
            let mut c = characters[p];
            let high = hex_value(characters.get(p + 1).copied().unwrap_or(-1));
            let low = hex_value(characters.get(p + 2).copied().unwrap_or(-1));
            if c == 37 && high >= 0 && low >= 0 {
                c = high * 16 + low;
                p += 2;
                changed = true;
            }
            characters[written] = c;
            ends[written] = ends.get(p).copied().unwrap_or(prefix_end);
            written += 1;
            p += 1;
        }
        characters.truncate(written);
        ends.truncate(written);
        if !changed {
            break;
        }
    }

    let character = characters.first().copied().unwrap_or(37);
    let first_end = ends.first().copied().unwrap_or(prefix_end);
    if character == 37 && limit < text.len() {
        // Only a contiguous percent/hex run can depend on text beyond the window. A non-hex literal
        // breaks the dependency: the percent before it can never decode on any later pass.
        for &c in characters.iter().skip(1) {
            if c != 37 && hex_value(c) < 0 {
                return Unit::new(character, first_end - start);
            }
        }
        return Unit {
            character,
            length: limit - start,
            refusal: true,
        };
    }
    Unit::new(character, first_end - start)
}

/// One logical character and its full source width, shared by token, delimiter and word-boundary
/// scans.
pub fn read_unit(text: &[u16], i: usize) -> Unit {
    let c = code_at(text, i);
    if c == 37 {
        let mut end = i + 1;
        // Collapse literal %25 layers in one forward walk before the bounded full-window decoder.
        // This is the fixpoint of that prefix, not a depth cap: even million-layer canonical chains
        // stay linear.
        while end + 1 < text.len() {
            let high = hex_value(code_at(text, end));
            let low = hex_value(code_at(text, end + 1));
            if high < 0 || low < 0 {
                break;
            }
            let decoded = high * 16 + low;
            end += 2;
            if decoded != 37 {
                return Unit::new(decoded, end - i);
            }
        }
        return percent_window(text, i, end);
    }
    if c == 38 {
        for &(entity, character) in HTML_ENTITIES {
            if matches_at(text, i, entity, text.len()) {
                return Unit::new(character, entity.len());
            }
        }
    }
    match c {
        0xff1a => Unit::new(58, 1),
        0x201c | 0x201d => Unit::new(34, 1),
        0x2018 | 0x2019 => Unit::new(39, 1),
        _ => Unit::new(c, 1),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Delimiter {
    pub end: usize,
    /// The first unresolved percent window; callers conservatively redact from here to the end.
    pub refusal_start: Option<usize>,
    /// One ":" or "=" between quotes, escapes and spaces, then an optional "[" or "{".
    pub separator: bool,
    pub opens_object: bool,
    pub brace_depth: i64,
    /// The end of the "=" padding it starts with, which belongs to the token before it.
    pub padding_end: usize,
    pub pair_equals: bool,
    pub cookie_gap: bool,
    pub space_only: bool,
    pub ends_with_lt: bool,
    pub closes_tag: bool,
    pub colon_only: bool,
    pub at: Option<usize>,
    pub ends_userinfo: bool,
    /// The quote character the delimiter ends with (it opens the value after it), or -1.
    pub quote: Code,
    /// How many backslashes escape that quote: the escaping depth its closing quote is written at.
    pub quote_backslashes: usize,
}

pub fn empty_delimiter() -> Delimiter {
    Delimiter {
        quote: -1,
        ..Delimiter::default()
    }
}

/// Read the delimiter that starts at i into d, one unit at a time.
pub fn read_delimiter(text: &[u16], i: usize, d: &mut Delimiter) {
    let mut position = i;
    let mut units = 0usize;
    let mut separators = 0usize;
    let mut separator_shape = true;
    let mut gap_marks = 0usize;
    let mut gap_shape = true;
    let mut padding = true;
    let mut spaces = 0usize;
    let mut backslashes = 0usize;
    let mut last: Code = -1;
    let mut first: Code = -1;
    let mut tag_shape = true;
    d.opens_object = false;
    d.brace_depth = 0;
    d.padding_end = i;
    d.at = None;
    d.ends_userinfo = false;
    d.quote = -1;
    d.quote_backslashes = 0;
    d.refusal_start = None;
    while position < text.len() && token_unit_length(text, position) == 0 {
        let unit = read_unit(text, position);
        if unit.refusal {
            d.refusal_start = Some(position);
            position = text.len();
            break;
        }
        let c = unit.character;
        if units == 0 {
            first = c;
        } else if !is_space(c) {
            tag_shape = false;
        }
        if padding && c == 61 {
            d.padding_end = position + unit.length;
        } else {
            padding = false;
            if c == 59 || c == 44 {
                gap_marks += 1;
            } else if !is_space(c) && !is_quote(c) {
                gap_shape = false;
            }
        }
        if c == 58 || c == 61 {
            separators += 1;
        } else if c == 123 {
            d.brace_depth += 1;
            if separators == 1 {
                d.opens_object = true;
            } else {
                separator_shape = false;
            }
        } else if c == 125 {
            d.brace_depth -= 1;
            separator_shape = false;
        } else if !is_space(c) && !is_quote(c) && !(c == 91 && separators == 1) {
            separator_shape = false;
        }
        if is_space(c) {
            spaces += 1;
        }
        if c == 64 && d.at.is_none() {
            d.at = Some(position);
        }
        if is_space(c) || c == 63 || c == 35 {
            d.ends_userinfo = true;
        }
        d.quote = if is_opening_quote(c) { c } else { -1 };
        d.quote_backslashes = backslashes;
        backslashes = if c == 92 { backslashes + 1 } else { 0 };
        last = c;
        units += 1;
        position += unit.length;
    }
    d.end = position;
    d.separator = separators == 1 && separator_shape && units <= MAX_SEPARATOR_LENGTH;
    d.pair_equals = units == 1 && first == 61;
    d.cookie_gap = gap_marks == 1 && gap_shape && units <= MAX_SEPARATOR_LENGTH;
    d.space_only = units > 0 && spaces == units && units <= MAX_SCHEME_GAP;
    d.ends_with_lt = last == 60;
    d.closes_tag = first == 62 && tag_shape;
    d.colon_only = units == 1 && first == 58;
}

/// Where a quoted value that continues at i ends: its closing quote, written at the opening
/// quote's escaping depth (a quote behind more backslashes is escaped inside the value), or the
/// end of the line.
pub fn quoted_value_end(text: &[u16], i: usize, quote: Code, depth: usize) -> usize {
    let mut position = i;
    let mut backslashes = 0usize;
    while position < text.len() {
        let raw = code_at(text, position);
        if raw == 10 || raw == 13 {
            return position;
        }
        if raw == 92 {
            backslashes += 1;
            position += 1;
            continue;
        }
        let unit = read_unit(text, position);
        if unit.refusal {
            return text.len();
        }
        if unit.character == quote && backslashes <= depth {
            return position - backslashes;
        }
        backslashes = 0;
        position += unit.length;
    }
    position
}

/// Where a bare value that continues at i ends: whitespace, ",", ";", "&", a quote or "<" (a
/// closing tag).
pub fn bare_value_end(text: &[u16], i: usize) -> usize {
    let mut position = i;
    while position < text.len() {
        let c = code_at(text, position);
        if is_space(c)
            || is_quote(c)
            || is_typographic_quote(c)
            || c == 44
            || c == 59
            || c == 38
            || c == 60
        {
            break;
        }
        position += 1;
    }
    position
}
