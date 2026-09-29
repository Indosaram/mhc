use serde_json::{Map, Value};

use super::scan::{Scanner, Token, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsoncParseError {
    pub message: String,
    pub offset: usize,
    pub length: usize,
}

impl JsoncParseError {
    fn new(message: &str, offset: usize, length: usize) -> Self {
        Self {
            message: message.to_string(),
            offset,
            length,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Object,
    Array,
    Property,
    String,
    Number,
    Boolean,
    Null,
}

#[derive(Debug, Clone)]
pub struct JsonNode {
    pub kind: NodeKind,
    pub offset: usize,
    pub length: usize,
    pub value: Option<String>,
    pub children: Vec<JsonNode>,
}

impl JsonNode {
    fn leaf(kind: NodeKind, token: Token, value: Option<String>) -> Self {
        Self {
            kind,
            offset: token.offset,
            length: token.length,
            value,
            children: Vec::new(),
        }
    }

    pub fn end(&self) -> usize {
        self.offset + self.length
    }
}

pub fn parse_tree(text: &str) -> Result<JsonNode, JsoncParseError> {
    let mut reader = Reader {
        scanner: Scanner::new(text),
        current: Token {
            kind: TokenKind::Eof,
            offset: 0,
            length: 0,
        },
    };
    reader.current = reader.scanner.scan_significant();
    let node = reader.parse_value()?;
    if reader.current.kind != TokenKind::Eof {
        return Err(JsoncParseError::new(
            "EndOfFileExpected",
            reader.current.offset,
            reader.current.length,
        ));
    }
    Ok(node)
}

struct Reader<'a> {
    scanner: Scanner<'a>,
    current: Token,
}

impl<'a> Reader<'a> {
    fn bump(&mut self) -> Token {
        let token = self.current;
        self.current = self.scanner.scan_significant();
        token
    }

    fn parse_value(&mut self) -> Result<JsonNode, JsoncParseError> {
        let token = self.current;
        match token.kind {
            TokenKind::OpenBrace => self.parse_object(),
            TokenKind::OpenBracket => self.parse_array(),
            TokenKind::String => {
                self.bump();
                let raw = &self.scanner.text()[token.offset..token.end()];
                Ok(JsonNode::leaf(
                    NodeKind::String,
                    token,
                    Some(decode_string(raw)),
                ))
            }
            TokenKind::Number => {
                self.bump();
                let raw = &self.scanner.text()[token.offset..token.end()];
                Ok(JsonNode::leaf(
                    NodeKind::Number,
                    token,
                    Some(raw.to_string()),
                ))
            }
            TokenKind::True | TokenKind::False => {
                self.bump();
                let text = if token.kind == TokenKind::True {
                    "true"
                } else {
                    "false"
                };
                Ok(JsonNode::leaf(
                    NodeKind::Boolean,
                    token,
                    Some(text.to_string()),
                ))
            }
            TokenKind::Null => {
                self.bump();
                Ok(JsonNode::leaf(NodeKind::Null, token, None))
            }
            TokenKind::Unknown => Err(JsoncParseError::new(
                "InvalidSymbol",
                token.offset,
                token.length,
            )),
            _ => Err(JsoncParseError::new("ValueExpected", token.offset, 0)),
        }
    }

    fn parse_property_name(&mut self) -> Result<(Token, String), JsoncParseError> {
        if self.current.kind != TokenKind::String {
            return Err(JsoncParseError::new(
                "PropertyNameExpected",
                self.current.offset,
                self.current.length,
            ));
        }
        let token = self.bump();
        let raw = &self.scanner.text()[token.offset..token.end()];
        Ok((token, decode_string(raw)))
    }

    fn parse_object(&mut self) -> Result<JsonNode, JsoncParseError> {
        let open = self.bump();
        let mut children: Vec<JsonNode> = Vec::new();
        let mut end: usize;
        if self.current.kind == TokenKind::CloseBrace {
            end = self.bump().end();
        } else {
            loop {
                let (key_token, key) = self.parse_property_name()?;
                if self.current.kind != TokenKind::Colon {
                    return Err(JsoncParseError::new(
                        "ColonExpected",
                        self.current.offset,
                        self.current.length,
                    ));
                }
                self.bump();
                let value = self.parse_value()?;
                let key_node = JsonNode::leaf(NodeKind::String, key_token, Some(key.clone()));
                end = value.end();
                children.push(JsonNode {
                    kind: NodeKind::Property,
                    offset: key_token.offset,
                    length: end - key_token.offset,
                    value: None,
                    children: vec![key_node, value],
                });
                match self.current.kind {
                    TokenKind::Comma => {
                        self.bump();
                        if self.current.kind == TokenKind::CloseBrace {
                            let close = self.bump();
                            end = close.end();
                            break;
                        }
                    }
                    TokenKind::CloseBrace => {
                        let close = self.bump();
                        end = close.end();
                        break;
                    }
                    TokenKind::Eof => {
                        return Err(JsoncParseError::new(
                            "CloseBraceExpected",
                            self.current.offset,
                            0,
                        ));
                    }
                    _ => {
                        return Err(JsoncParseError::new(
                            "CommaExpected",
                            self.current.offset,
                            self.current.length,
                        ));
                    }
                }
            }
        }
        Ok(JsonNode {
            kind: NodeKind::Object,
            offset: open.offset,
            length: end - open.offset,
            value: None,
            children,
        })
    }

    fn parse_array(&mut self) -> Result<JsonNode, JsoncParseError> {
        let open = self.bump();
        let mut children: Vec<JsonNode> = Vec::new();
        let end: usize;
        if self.current.kind == TokenKind::CloseBracket {
            end = self.bump().end();
        } else {
            loop {
                let value = self.parse_value()?;
                children.push(value);
                match self.current.kind {
                    TokenKind::Comma => {
                        self.bump();
                        if self.current.kind == TokenKind::CloseBracket {
                            let close = self.bump();
                            end = close.end();
                            break;
                        }
                    }
                    TokenKind::CloseBracket => {
                        let close = self.bump();
                        end = close.end();
                        break;
                    }
                    TokenKind::Eof => {
                        return Err(JsoncParseError::new(
                            "CloseBracketExpected",
                            self.current.offset,
                            0,
                        ));
                    }
                    _ => {
                        return Err(JsoncParseError::new(
                            "CommaExpected",
                            self.current.offset,
                            self.current.length,
                        ));
                    }
                }
            }
        }
        Ok(JsonNode {
            kind: NodeKind::Array,
            offset: open.offset,
            length: end - open.offset,
            value: None,
            children,
        })
    }
}

pub fn node_path_segment(node: &JsonNode) -> Option<&str> {
    node.children.first().and_then(|key| key.value.as_deref())
}

pub fn find_node_at_location<'a>(
    root: &'a JsonNode,
    path: &[crate::issue::PathSegment],
) -> Option<&'a JsonNode> {
    let mut node = root;
    for segment in path {
        match segment {
            crate::issue::PathSegment::Key(key) => {
                if node.kind != NodeKind::Object {
                    return None;
                }
                let mut found = None;
                for child in &node.children {
                    if child.kind == NodeKind::Property
                        && node_path_segment(child) == Some(key.as_str())
                    {
                        found = child.children.get(1);
                        break;
                    }
                }
                node = found?;
            }
            crate::issue::PathSegment::Index(index) => {
                if node.kind != NodeKind::Array || *index < 0 {
                    return None;
                }
                node = node.children.get(*index as usize)?;
            }
        }
    }
    Some(node)
}

pub fn to_value(node: &JsonNode) -> Value {
    match node.kind {
        NodeKind::Object => {
            let mut map = Map::new();
            for child in &node.children {
                if child.kind != NodeKind::Property {
                    continue;
                }
                let Some(key) = node_path_segment(child) else {
                    continue;
                };
                let Some(value) = child.children.get(1) else {
                    continue;
                };
                map.insert(key.to_string(), to_value(value));
            }
            Value::Object(map)
        }
        NodeKind::Array => Value::Array(node.children.iter().map(to_value).collect()),
        NodeKind::String => Value::String(node.value.clone().unwrap_or_default()),
        NodeKind::Number => node
            .value
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap_or(Value::Null),
        NodeKind::Boolean => Value::Bool(node.value.as_deref() == Some("true")),
        NodeKind::Null | NodeKind::Property => Value::Null,
    }
}

fn decode_string(raw: &str) -> String {
    let body = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(raw);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.char_indices();
    while let Some((index, ch)) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some((digit_index, escape)) = chars.next() else {
            break;
        };
        let _ = digit_index;
        match escape {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let start = index + 2;
                let hex = body.get(start..start + 4).unwrap_or("");
                let unit = u32::from_str_radix(hex, 16).unwrap_or(0xfffd);
                if (0xd800..0xdc00).contains(&unit) {
                    let low_start = start + 6;
                    let low_hex = body.get(low_start..low_start + 4).unwrap_or("");
                    let low = u32::from_str_radix(low_hex, 16).unwrap_or(0);
                    if (0xdc00..0xe000).contains(&low) {
                        let combined = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                        out.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                        for _ in 0..6 {
                            chars.next();
                        }
                        continue;
                    }
                    out.push('\u{fffd}');
                    continue;
                }
                out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
                for _ in 0..4 {
                    chars.next();
                }
            }
            other => out.push(other),
        }
    }
    out
}
