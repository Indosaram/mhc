//! Strict canonical JSON parsing for signed sync preimages.

pub use super::canonical_json_serialize::canonicalize_json;
use super::canonical_json_serialize::{canonical_number, compare_utf16, escape_canonical_string};
pub use super::canonical_json_types::{
    CanonicalJsonError, CanonicalJsonValue, CanonicalJsonViolation,
};
use CanonicalJsonViolation as Violation;
use serde_json::{Map, Number, Value};

pub const MAX_CANONICAL_DEPTH: usize = 64;

struct Parser<'a> {
    text: &'a str,
    position: usize,
    depth: usize,
}

impl Parser<'_> {
    fn parse_document(&mut self) -> Result<Value, CanonicalJsonError> {
        let value = self.parse_value()?;
        self.skip_insignificant()?;
        if self.position != self.text.len() {
            return Err(self.fail(Violation::Json, "trailing data after the JSON document"));
        }
        Ok(value)
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.position).copied()
    }

    fn parse_value(&mut self) -> Result<Value, CanonicalJsonError> {
        self.skip_insignificant()?;
        match self.peek() {
            Some(b'{' | b'[') => {
                if self.depth >= MAX_CANONICAL_DEPTH {
                    return Err(self.fail(Violation::Json, "nesting deeper than 64"));
                }
                self.depth += 1;
                let result = if self.peek() == Some(b'{') {
                    self.parse_object()
                } else {
                    self.parse_array()
                };
                self.depth -= 1;
                result
            }
            Some(b'"') => self.parse_string(true).map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => {
                for (word, value) in [
                    ("null", Value::Null),
                    ("true", Value::Bool(true)),
                    ("false", Value::Bool(false)),
                ] {
                    if self.text[self.position..].starts_with(word) {
                        self.position += word.len();
                        return Ok(value);
                    }
                }
                Err(self.fail(Violation::Json, "unexpected token"))
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, CanonicalJsonError> {
        self.position += 1;
        let mut object = Map::new();
        let mut keys = Vec::new();
        self.skip_insignificant()?;
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Value::Object(object));
        }
        loop {
            self.skip_insignificant()?;
            if self.peek() != Some(b'"') {
                return Err(self.fail(Violation::Json, "object keys must be strings"));
            }
            let start = self.position;
            let key = self.parse_string(false)?;
            if object.contains_key(&key) {
                return Err(self.fail(Violation::DuplicateKey, "duplicate property name"));
            }
            if escape_canonical_string(&key) != self.text[start..self.position] {
                return Err(self.fail(Violation::Escape, "string is not minimally escaped"));
            }
            keys.push(key.clone());
            self.skip_insignificant()?;
            if self.peek() != Some(b':') {
                return Err(self.fail(Violation::Json, "expected ':' after object key"));
            }
            self.position += 1;
            object.insert(key, self.parse_value()?);
            self.skip_insignificant()?;
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    break;
                }
                _ => return Err(self.fail(Violation::Json, "expected ',' or '}' in object")),
            }
        }
        if keys
            .windows(2)
            .any(|pair| !compare_utf16(&pair[0], &pair[1]).is_lt())
        {
            return Err(self.fail(Violation::Order, "object properties out of order"));
        }
        Ok(Value::Object(object))
    }

    fn parse_array(&mut self) -> Result<Value, CanonicalJsonError> {
        self.position += 1;
        let mut array = Vec::new();
        self.skip_insignificant()?;
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Value::Array(array));
        }
        loop {
            array.push(self.parse_value()?);
            self.skip_insignificant()?;
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    break;
                }
                _ => return Err(self.fail(Violation::Json, "expected ',' or ']' in array")),
            }
        }
        Ok(Value::Array(array))
    }

    fn parse_string(&mut self, check_escape: bool) -> Result<String, CanonicalJsonError> {
        let start = self.position;
        self.position += 1;
        let mut units = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.fail(Violation::Json, "unterminated string")),
                Some(b'"') => {
                    self.position += 1;
                    let value = String::from_utf16(&units)
                        .map_err(|_| self.fail(Violation::Encoding, "lone surrogate in string"))?;
                    if check_escape
                        && escape_canonical_string(&value) != self.text[start..self.position]
                    {
                        return Err(self.fail(Violation::Escape, "string is not minimally escaped"));
                    }
                    return Ok(value);
                }
                Some(b'\\') => {
                    self.position += 1;
                    let unit = match self.peek() {
                        Some(b'"') => 0x22,
                        Some(b'\\') => 0x5c,
                        Some(b'/') => 0x2f,
                        Some(b'b') => 8,
                        Some(b'f') => 12,
                        Some(b'n') => 10,
                        Some(b'r') => 13,
                        Some(b't') => 9,
                        Some(b'u') => {
                            self.position += 1;
                            let mut unit = 0u16;
                            for _ in 0..4 {
                                let digit = self
                                    .peek()
                                    .and_then(|byte| char::from(byte).to_digit(16))
                                    .and_then(|digit| u16::try_from(digit).ok())
                                    .ok_or_else(|| {
                                        self.fail(Violation::Json, "invalid \\u escape")
                                    })?;
                                unit = unit * 16 + digit;
                                self.position += 1;
                            }
                            units.push(unit);
                            continue;
                        }
                        _ => return Err(self.fail(Violation::Json, "invalid escape sequence")),
                    };
                    units.push(unit);
                    self.position += 1;
                }
                Some(0..=31) => {
                    return Err(self.fail(Violation::Json, "unescaped control character in string"));
                }
                Some(_) => {
                    let character = self.text[self.position..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.fail(Violation::Json, "unterminated string"))?;
                    units.extend(character.encode_utf16(&mut [0; 2]).iter().copied());
                    self.position += character.len_utf8();
                }
            }
        }
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
    }

    fn parse_number(&mut self) -> Result<Value, CanonicalJsonError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.fail(Violation::Json, "invalid number")),
        }
        if self.peek() == Some(b'.')
            && self
                .text
                .as_bytes()
                .get(self.position + 1)
                .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let exponent = self.position;
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            let digits = self.position;
            self.digits();
            if digits == self.position {
                self.position = exponent;
            }
        }
        let raw = &self.text[start..self.position];
        let value: f64 = raw
            .parse()
            .map_err(|_| self.fail(Violation::Number, "invalid number"))?;
        if !value.is_finite() {
            return Err(self.fail(Violation::Number, "number is not finite"));
        }
        if canonical_number(value) != raw {
            return Err(self.fail(Violation::Number, "number is not in canonical form"));
        }
        // Keep canonical integer tokens usable by typed epoch/version consumers.
        // Binary64 canonicality was already checked above, before representation.
        raw.parse::<Number>()
            .map(Value::Number)
            .map_err(|_| self.fail(Violation::Number, "invalid number"))
    }

    fn skip_insignificant(&self) -> Result<(), CanonicalJsonError> {
        if matches!(self.peek(), Some(b'\t' | b'\n' | b'\r' | b' ')) {
            return Err(self.fail(
                Violation::Whitespace,
                "insignificant whitespace between JSON tokens",
            ));
        }
        Ok(())
    }

    fn fail(&self, violation: Violation, message: &str) -> CanonicalJsonError {
        let offset = self.text[..self.position].encode_utf16().count();
        CanonicalJsonError::new(violation, format!("{message} (offset {offset})"))
    }
}

pub fn require_canonical_json(bytes: &[u8]) -> Result<CanonicalJsonValue, CanonicalJsonError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        CanonicalJsonError::new(Violation::Encoding, "canonical JSON must be valid UTF-8")
    })?;
    // TextDecoder strips a leading UTF-8 BOM; byte equality below still refuses it.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let parsed = Parser {
        text,
        position: 0,
        depth: 0,
    }
    .parse_document()?;
    if canonicalize_json(&parsed)? != bytes {
        return Err(CanonicalJsonError::new(
            Violation::Json,
            "input is not byte-identical to its canonical form",
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
#[path = "canonical_json_tests.rs"]
mod tests;
