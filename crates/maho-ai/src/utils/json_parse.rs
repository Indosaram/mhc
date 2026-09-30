//! Port of senpi packages/ai/src/utils/json-parse.ts, including the `partial-json` 0.1.7
//! parser it calls (with its default `Allow.ALL`).

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

fn is_valid_json_escape(c: char) -> bool {
    matches!(c, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')
}

fn escape_control_character(c: char) -> String {
    match c {
        '\u{8}' => "\\b".into(),
        '\u{c}' => "\\f".into(),
        '\n' => "\\n".into(),
        '\r' => "\\r".into(),
        '\t' => "\\t".into(),
        other => format!("\\u{:04x}", other as u32),
    }
}

/// Escapes raw control characters and invalid backslashes inside JSON strings.
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if !in_string {
            repaired.push(c);
            in_string = c == '"';
        } else if c == '"' {
            repaired.push(c);
            in_string = false;
        } else if c == '\\' {
            match chars.get(index + 1).copied() {
                None => repaired.push_str("\\\\"),
                Some('u') if chars.len() >= index + 6 && chars[index + 2..index + 6].iter().all(char::is_ascii_hexdigit) => {
                    repaired.push_str("\\u");
                    repaired.extend(&chars[index + 2..index + 6]);
                    index += 5;
                }
                Some(next) if is_valid_json_escape(next) => {
                    repaired.push('\\');
                    repaired.push(next);
                    index += 1;
                }
                Some(_) => repaired.push_str("\\\\"),
            }
        } else if (c as u32) <= 0x1f {
            repaired.push_str(&escape_control_character(c));
        } else {
            repaired.push(c);
        }
        index += 1;
    }
    repaired
}

pub fn parse_json_with_repair<T: DeserializeOwned>(json: &str) -> Result<T, serde_json::Error> {
    match serde_json::from_str(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired = repair_json(json);
            if repaired != json { serde_json::from_str(&repaired) } else { Err(error) }
        }
    }
}

/// Parses possibly incomplete streamed tool-call JSON; anything unparseable yields `{}`.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(partial_json) = partial_json.filter(|json| !crate::utils::js::trim(json).is_empty()) else {
        return Value::Object(Map::new());
    };
    let or_empty = |value: Value| if value.is_null() { Value::Object(Map::new()) } else { value };
    parse_json_with_repair::<Value>(partial_json)
        .ok()
        .or_else(|| partial_parse(partial_json).ok().map(or_empty))
        .or_else(|| partial_parse(&repair_json(partial_json)).ok().map(or_empty))
        .unwrap_or_else(|| Value::Object(Map::new()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartialJsonError {
    Partial(String),
    Malformed(String),
}

/// `partial-json` `parse(text)` with `Allow.ALL`. JS-only values (`NaN`, `Infinity`) become
/// `null`, which is what they serialize to.
pub fn partial_parse(json: &str) -> Result<Value, PartialJsonError> {
    let trimmed = crate::utils::js::trim(json);
    if trimmed.is_empty() {
        return Err(PartialJsonError::Malformed(format!("{json} is empty")));
    }
    let mut parser = PartialParser { s: trimmed.chars().collect(), index: 0 };
    parser.parse_any()
}

struct PartialParser {
    s: Vec<char>,
    index: usize,
}

impl PartialParser {
    fn len(&self) -> usize {
        self.s.len()
    }

    fn at(&self, index: usize) -> Option<char> {
        self.s.get(index).copied()
    }

    /// JS `substring(a, b)`: clamped and swapped.
    fn substring(&self, a: isize, b: isize) -> String {
        let clamp = |v: isize| v.clamp(0, self.len() as isize) as usize;
        let (a, b) = (clamp(a), clamp(b));
        let (a, b) = if a > b { (b, a) } else { (a, b) };
        self.s[a..b].iter().collect()
    }

    fn last_index_of(&self, c: char) -> isize {
        self.s.iter().rposition(|&x| x == c).map_or(-1, |i| i as isize)
    }

    fn partial(&self, message: &str) -> PartialJsonError {
        PartialJsonError::Partial(format!("{message} at position {}", self.index))
    }

    fn malformed(&self, message: &str) -> PartialJsonError {
        PartialJsonError::Malformed(format!("{message} at position {}", self.index))
    }

    fn json(&self, text: &str) -> Result<Value, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    fn literal(&mut self, word: &str) -> bool {
        let rest = self.substring(self.index as isize, self.len() as isize);
        let word_len = word.chars().count();
        let remaining = self.len() - self.index;
        let full = self.substring(self.index as isize, (self.index + word_len) as isize) == word;
        let min_remaining = if word == "-Infinity" { 2 } else { 0 };
        if full || (remaining < word_len && remaining >= min_remaining && word.starts_with(rest.as_str())) {
            self.index += word_len;
            return true;
        }
        false
    }

    fn parse_any(&mut self) -> Result<Value, PartialJsonError> {
        self.skip_blank();
        match self.at(self.index) {
            None => Err(self.partial("Unexpected end of input")),
            Some('"') => self.parse_str(),
            Some('{') => self.parse_obj(),
            Some('[') => self.parse_arr(),
            Some(_) => {
                if self.literal("null") {
                    return Ok(Value::Null);
                }
                if self.literal("true") {
                    return Ok(Value::Bool(true));
                }
                if self.literal("false") {
                    return Ok(Value::Bool(false));
                }
                if self.literal("Infinity") || self.literal("-Infinity") || self.literal("NaN") {
                    return Ok(Value::Null);
                }
                self.parse_num()
            }
        }
    }

    fn parse_str(&mut self) -> Result<Value, PartialJsonError> {
        let start = self.index;
        let mut escape = false;
        self.index += 1;
        while self.index < self.len() && (self.s[self.index] != '"' || (escape && self.s[self.index - 1] == '\\')) {
            escape = if self.s[self.index] == '\\' { !escape } else { false };
            self.index += 1;
        }
        let escape_offset = isize::from(escape);
        if self.at(self.index) == Some('"') {
            self.index += 1;
            let text = self.substring(start as isize, self.index as isize - escape_offset);
            return self.json(&text).map_err(|e| self.malformed(&e));
        }
        let text = self.substring(start as isize, self.index as isize - escape_offset) + "\"";
        match self.json(&text) {
            Ok(value) => Ok(value),
            Err(_) => {
                let text = self.substring(start as isize, self.last_index_of('\\')) + "\"";
                self.json(&text).map_err(|e| self.malformed(&e))
            }
        }
    }

    fn parse_obj(&mut self) -> Result<Value, PartialJsonError> {
        self.index += 1;
        self.skip_blank();
        let mut object = Map::new();
        while self.at(self.index) != Some('}') {
            self.skip_blank();
            if self.index >= self.len() {
                return Ok(Value::Object(object));
            }
            let Ok(key) = self.parse_str() else { return Ok(Value::Object(object)) };
            let key = match key {
                Value::String(key) => key,
                other => other.to_string(),
            };
            self.skip_blank();
            self.index += 1;
            match self.parse_any() {
                Ok(value) => {
                    object.insert(key, value);
                }
                Err(_) => return Ok(Value::Object(object)),
            }
            self.skip_blank();
            if self.at(self.index) == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1;
        Ok(Value::Object(object))
    }

    fn parse_arr(&mut self) -> Result<Value, PartialJsonError> {
        self.index += 1;
        let mut array = Vec::new();
        while self.at(self.index) != Some(']') {
            match self.parse_any() {
                Ok(value) => array.push(value),
                Err(_) => return Ok(Value::Array(array)),
            }
            self.skip_blank();
            if self.at(self.index) == Some(',') {
                self.index += 1;
            }
        }
        self.index += 1;
        Ok(Value::Array(array))
    }

    fn parse_num(&mut self) -> Result<Value, PartialJsonError> {
        if self.index == 0 {
            let whole: String = self.s.iter().collect();
            if whole == "-" {
                return Err(self.malformed("Not sure what '-' is"));
            }
            return match self.json(&whole) {
                Ok(value) => Ok(value),
                Err(error) => {
                    let head = self.substring(0, self.last_index_of('e'));
                    self.json(&head).map_err(|_| self.malformed(&error))
                }
            };
        }
        let start = self.index;
        if self.at(self.index) == Some('-') {
            self.index += 1;
        }
        while self.at(self.index).is_some_and(|c| !",]}".contains(c)) {
            self.index += 1;
        }
        let text = self.substring(start as isize, self.index as isize);
        match self.json(&text) {
            Ok(value) => Ok(value),
            Err(_) if text == "-" => Err(self.partial("Not sure what '-' is")),
            Err(_) => {
                let head = self.substring(start as isize, self.last_index_of('e'));
                self.json(&head).map_err(|e| self.malformed(&e))
            }
        }
    }

    fn skip_blank(&mut self) {
        while self.at(self.index).is_some_and(|c| matches!(c, ' ' | '\n' | '\r' | '\t')) {
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repairs_control_characters_and_bad_escapes() {
        assert_eq!(repair_json("{\"a\":\"x\ny\\q\\u00e9\"}"), "{\"a\":\"x\\ny\\\\q\\u00e9\"}");
        let value: Value = parse_json_with_repair("{\"path\":\"C:\\dir\ttab\"}").expect("repaired");
        assert_eq!(value, json!({"path": "C:\\dir\ttab"}));
        assert!(parse_json_with_repair::<Value>("{").is_err());
    }

    #[test]
    fn streaming_json_parses_partial_prefixes() {
        assert_eq!(parse_streaming_json(None), json!({}));
        assert_eq!(parse_streaming_json(Some("  ")), json!({}));
        assert_eq!(parse_streaming_json(Some(r#"{"a": 1, "b": "hel"#)), json!({"a": 1, "b": "hel"}));
        assert_eq!(parse_streaming_json(Some(r#"{"a": [1, 2, {"c": tr"#)), json!({"a": [1, 2, {"c": true}]}));
        assert_eq!(parse_streaming_json(Some(r#"{"a": 12"#)), json!({"a": 12}));
        assert_eq!(parse_streaming_json(Some(r#"{"a": "x\u12"#)), json!({"a": "x"}));
        // senpi fe8c564b: an unterminated string with a raw newline yields {}; the closed one repairs.
        assert_eq!(parse_streaming_json(Some("{\"a\": \"line\nbreak")), json!({}));
        assert_eq!(parse_streaming_json(Some("{\"a\": \"line\nbreak\"}")), json!({"a": "line\nbreak"}));
        assert_eq!(parse_streaming_json(Some(r#"{"k"#)), json!({}));
        // senpi fe8c564b: `parseJsonWithRepair` succeeds on "null" and its result is returned
        // as-is (only the partial-json fallback path defaults nullish results to {}).
        assert_eq!(parse_streaming_json(Some("null")), Value::Null);
    }

    #[test]
    fn partial_parse_errors() {
        assert!(matches!(partial_parse("-"), Err(PartialJsonError::Malformed(_))));
        assert!(matches!(partial_parse("\"abc"), Ok(Value::String(s)) if s == "abc"));
        assert_eq!(partial_parse("[1, -"), Ok(json!([1])));
    }
}
