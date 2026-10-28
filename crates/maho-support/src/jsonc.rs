//! JSON-with-comments reading.
//!
//! The parser accepts `//` and `/* */` comments and trailing commas, and keeps going after a
//! syntax error so that every error in a document is reported with its byte offset.

use serde_json::{Map, Number, Value};
use std::path::Path;

/// One syntax error with the byte offset where it was detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsoncError {
    pub message: String,
    pub offset: usize,
}

impl std::fmt::Display for JsoncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at offset {}", self.message, self.offset)
    }
}

impl std::error::Error for JsoncError {}

/// Result of a lenient parse: the best-effort value and every syntax error found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsoncParse {
    /// `None` for empty (or comment/whitespace-only) input.
    pub value: Option<Value>,
    pub errors: Vec<JsoncError>,
}

impl JsoncParse {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Parses JSONC, collecting all syntax errors instead of stopping at the first one.
///
/// When `errors` is non-empty, `value` is only a best-effort reconstruction and callers that
/// need a trustworthy document must treat the input as invalid.
pub fn parse_jsonc_lenient(text: &str) -> JsoncParse {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        errors: Vec::new(),
        depth: 0,
    };
    parser.skip_trivia();
    if parser.at_end() {
        return JsoncParse {
            value: None,
            errors: parser.errors,
        };
    }
    let value = parser.parse_value();
    parser.skip_trivia();
    if !parser.at_end() {
        let offset = parser.pos;
        parser.error("unexpected content after the top-level value", offset);
    }
    JsoncParse {
        value,
        errors: parser.errors,
    }
}

/// Parses JSONC strictly: the first syntax error (or an empty document) is an error.
pub fn parse_jsonc(text: &str) -> Result<Value, JsoncError> {
    let parsed = parse_jsonc_lenient(text);
    if let Some(error) = parsed.errors.into_iter().next() {
        return Err(error);
    }
    parsed.value.ok_or(JsoncError {
        message: "empty document".into(),
        offset: 0,
    })
}

/// Reads and parses a JSONC file. `None` when the file is missing, unreadable, empty or has any
/// syntax error. Never panics.
pub fn read_jsonc_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = parse_jsonc_lenient(&text);
    if parsed.errors.is_empty() {
        parsed.value
    } else {
        None
    }
}

const MAX_DEPTH: usize = 512;

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    errors: Vec<JsoncError>,
    depth: usize,
}

impl Parser<'_> {
    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn error(&mut self, message: &str, offset: usize) {
        self.errors.push(JsoncError {
            message: message.into(),
            offset,
        });
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(0xEF)
                    if self.pos == 0 && self.bytes.get(..3) == Some(&[0xEF, 0xBB, 0xBF][..]) =>
                {
                    self.pos += 3;
                }
                Some(b'/') => match self.bytes.get(self.pos + 1) {
                    Some(b'/') => {
                        while let Some(byte) = self.peek() {
                            if byte == b'\n' {
                                break;
                            }
                            self.pos += 1;
                        }
                    }
                    Some(b'*') => {
                        let start = self.pos;
                        self.pos += 2;
                        let mut closed = false;
                        while !self.at_end() {
                            if self.bytes[self.pos] == b'*'
                                && self.bytes.get(self.pos + 1) == Some(&b'/')
                            {
                                self.pos += 2;
                                closed = true;
                                break;
                            }
                            self.pos += 1;
                        }
                        if !closed {
                            self.error("unterminated block comment", start);
                        }
                    }
                    _ => return,
                },
                _ => return,
            }
        }
    }

    /// Skips to the next `,` or the closing `close` byte at the current nesting level without
    /// consuming it. Strings and comments are skipped as units.
    fn recover(&mut self) {
        let mut nesting: Vec<u8> = Vec::new();
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.skip_string_raw();
                    continue;
                }
                b'/' if matches!(self.bytes.get(self.pos + 1), Some(b'/' | b'*')) => {
                    self.skip_trivia();
                    continue;
                }
                b'{' => nesting.push(b'}'),
                b'[' => nesting.push(b']'),
                b'}' | b']' => {
                    match nesting.last() {
                        Some(expected) if *expected == byte => {
                            nesting.pop();
                        }
                        Some(_) => {}
                        // Our own closer (`close`) or an outer one: leave it for the caller.
                        None => return,
                    }
                }
                b',' if nesting.is_empty() => return,
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn skip_string_raw(&mut self) {
        self.pos += 1;
        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'\\' => self.pos += 1,
                b'"' => return,
                b'\n' => return,
                _ => {}
            }
        }
    }

    fn parse_value(&mut self) -> Option<Value> {
        let start = self.pos;
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(b't' | b'f' | b'n') => self.parse_literal(),
            Some(_) => {
                self.error("expected a value", start);
                // Consume one word-like run so recovery always makes progress.
                let mut consumed = false;
                while let Some(byte) = self.peek() {
                    if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
                    {
                        self.pos += 1;
                        consumed = true;
                    } else {
                        break;
                    }
                }
                if !consumed
                    && !matches!(self.peek(), Some(b',' | b'}' | b']' | b':'))
                    && !self.at_end()
                {
                    self.pos += 1;
                }
                None
            }
            None => {
                self.error("unexpected end of input, expected a value", start);
                None
            }
        }
    }

    fn enter(&mut self) -> bool {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            let offset = self.pos;
            self.error("nesting too deep", offset);
            return false;
        }
        true
    }

    fn parse_object(&mut self) -> Option<Value> {
        let open = self.pos;
        self.pos += 1;
        if !self.enter() {
            self.pos = self.bytes.len();
            return None;
        }
        let mut map = Map::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => {
                    self.error("unterminated object", open);
                    break;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b'"') => {}
                Some(_) => {
                    let offset = self.pos;
                    self.error("expected a property name", offset);
                    self.recover();
                    if self.consume_separator(b'}', open) {
                        continue;
                    }
                    break;
                }
            }
            let key = self.parse_string().unwrap_or_default();
            self.skip_trivia();
            if self.peek() == Some(b':') {
                self.pos += 1;
            } else {
                let offset = self.pos;
                self.error("expected ':' after a property name", offset);
                self.recover();
                if self.consume_separator(b'}', open) {
                    continue;
                }
                break;
            }
            self.skip_trivia();
            match self.peek() {
                Some(b',' | b'}') | None => {
                    let offset = self.pos;
                    self.error("expected a value", offset);
                }
                Some(_) => {
                    if let Some(value) = self.parse_value() {
                        map.insert(key, value);
                    }
                }
            }
            self.skip_trivia();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    self.error("unterminated object", open);
                    break;
                }
                Some(_) => {
                    let offset = self.pos;
                    self.error("expected ',' or '}'", offset);
                    self.recover();
                    if !self.consume_separator(b'}', open) {
                        break;
                    }
                }
            }
        }
        self.depth -= 1;
        Some(Value::Object(map))
    }

    /// After recovery: consumes a `,` (returns true to continue) or the closer (returns false).
    fn consume_separator(&mut self, close: u8, open: usize) -> bool {
        match self.peek() {
            Some(b',') => {
                self.pos += 1;
                true
            }
            Some(byte) if byte == close => {
                self.pos += 1;
                false
            }
            Some(_) => false,
            None => {
                let message = if close == b'}' {
                    "unterminated object"
                } else {
                    "unterminated array"
                };
                self.error(message, open);
                false
            }
        }
    }

    fn parse_array(&mut self) -> Option<Value> {
        let open = self.pos;
        self.pos += 1;
        if !self.enter() {
            self.pos = self.bytes.len();
            return None;
        }
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => {
                    self.error("unterminated array", open);
                    break;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    let offset = self.pos;
                    self.error("expected a value", offset);
                    self.pos += 1;
                    continue;
                }
                Some(_) => {
                    if let Some(value) = self.parse_value() {
                        items.push(value);
                    }
                }
            }
            self.skip_trivia();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    self.error("unterminated array", open);
                    break;
                }
                Some(_) => {
                    let offset = self.pos;
                    self.error("expected ',' or ']'", offset);
                    self.recover();
                    if !self.consume_separator(b']', open) {
                        break;
                    }
                }
            }
        }
        self.depth -= 1;
        Some(Value::Array(items))
    }

    fn parse_string(&mut self) -> Option<String> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        let mut segment_start = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                self.error("unterminated string", start);
                return None;
            };
            match byte {
                b'"' => {
                    out.push_str(&self.text[segment_start..self.pos]);
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    out.push_str(&self.text[segment_start..self.pos]);
                    let escape_at = self.pos;
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            self.pos += 1;
                            match self.read_unicode_escape() {
                                Some(ch) => out.push(ch),
                                None => self.error("invalid unicode escape", escape_at),
                            }
                            segment_start = self.pos;
                            continue;
                        }
                        _ => self.error("invalid escape sequence", escape_at),
                    }
                    self.pos += 1;
                    segment_start = self.pos;
                }
                b'\n' | b'\r' => {
                    self.error("unterminated string", start);
                    out.push_str(&self.text[segment_start..self.pos]);
                    return Some(out);
                }
                _ => {
                    // Advance one UTF-8 scalar at a time to keep slicing on char boundaries.
                    let len = utf8_len(byte);
                    self.pos = (self.pos + len).min(self.bytes.len());
                }
            }
        }
    }

    fn read_hex4(&mut self) -> Option<u32> {
        let digits = self.text.get(self.pos..self.pos + 4)?;
        let value = u32::from_str_radix(digits, 16).ok()?;
        self.pos += 4;
        Some(value)
    }

    fn read_unicode_escape(&mut self) -> Option<char> {
        let first = self.read_hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            if self.bytes.get(self.pos) == Some(&b'\\')
                && self.bytes.get(self.pos + 1) == Some(&b'u')
            {
                let save = self.pos;
                self.pos += 2;
                if let Some(second) = self.read_hex4()
                    && (0xDC00..0xE000).contains(&second)
                {
                    let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                    return char::from_u32(combined);
                }
                self.pos = save;
            }
            return Some('\u{FFFD}');
        }
        char::from_u32(first).or(Some('\u{FFFD}'))
    }

    fn parse_number(&mut self) -> Option<Value> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        let mut ok = self.pos > digits_start;
        if ok && self.bytes[digits_start] == b'0' && self.pos - digits_start > 1 {
            ok = false;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let frac = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            ok &= self.pos > frac;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            ok &= self.pos > exp;
        }
        // Reject trailing identifier characters glued to the number (e.g. `12abc`).
        while let Some(byte) = self.peek() {
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' {
                ok = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        if !ok {
            self.error("invalid number", start);
            return None;
        }
        let slice = &self.text[start..self.pos];
        match serde_json::from_str::<Number>(slice) {
            Ok(number) => Some(Value::Number(number)),
            Err(_) => slice
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map(Value::Number)
                .or_else(|| {
                    self.error("invalid number", start);
                    None
                }),
        }
    }

    fn parse_literal(&mut self) -> Option<Value> {
        let start = self.pos;
        while matches!(self.peek(), Some(byte) if byte.is_ascii_alphanumeric() || byte == b'_') {
            self.pos += 1;
        }
        match &self.text[start..self.pos] {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            "null" => Some(Value::Null),
            _ => {
                self.error("expected a value", start);
                None
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
