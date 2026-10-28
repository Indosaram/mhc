//! A span-keeping view of a (syntactically valid) JSONC document, used by the writer to edit
//! text in place without disturbing comments or formatting.

/// A value with its byte span.
#[derive(Clone, Debug)]
pub struct Node {
    pub start: usize,
    pub end: usize,
    pub kind: NodeKind,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    Scalar,
}

/// One `"key": value` member of an object.
#[derive(Clone, Debug)]
pub struct Member {
    pub key: String,
    pub key_start: usize,
    pub value: Node,
    /// Byte offset of the comma that follows the member, if any.
    pub comma: Option<usize>,
}

/// Parses `text` into a span tree. Returns `None` on any syntax problem (callers validate the
/// text with the JSONC parser first, so this only guards against surprises).
pub fn parse(text: &str) -> Option<Node> {
    let mut parser = SpanParser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
    };
    parser.trivia();
    let node = parser.value()?;
    parser.trivia();
    (parser.pos == parser.bytes.len()).then_some(node)
}

struct SpanParser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl SpanParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.pos += 1,
                Some(0xEF) if self.bytes[self.pos..].starts_with(&[0xEF, 0xBB, 0xBF]) => {
                    self.pos += 3
                }
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    while let Some(byte) = self.peek() {
                        if byte == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += 2;
                    while self.pos < self.bytes.len() && !self.bytes[self.pos..].starts_with(b"*/")
                    {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.bytes.len());
                }
                _ => return,
            }
        }
    }

    fn value(&mut self) -> Option<Node> {
        let start = self.pos;
        match self.peek()? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => {
                self.string()?;
                Some(Node {
                    start,
                    end: self.pos,
                    kind: NodeKind::Scalar,
                })
            }
            _ => {
                while let Some(byte) = self.peek() {
                    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                (self.pos > start).then_some(Node {
                    start,
                    end: self.pos,
                    kind: NodeKind::Scalar,
                })
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        let start = self.pos;
        self.pos += 1;
        while let Some(byte) = self.peek() {
            match byte {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    let raw = self.text.get(start..self.pos)?;
                    return serde_json::from_str::<String>(raw).ok();
                }
                _ => self.pos += 1,
            }
        }
        None
    }

    fn object(&mut self) -> Option<Node> {
        let start = self.pos;
        self.pos += 1;
        let mut members = Vec::new();
        loop {
            self.trivia();
            match self.peek()? {
                b'}' => {
                    self.pos += 1;
                    return Some(Node {
                        start,
                        end: self.pos,
                        kind: NodeKind::Object(members),
                    });
                }
                b'"' => {
                    let key_start = self.pos;
                    let key = self.string()?;
                    self.trivia();
                    if self.peek()? != b':' {
                        return None;
                    }
                    self.pos += 1;
                    self.trivia();
                    let value = self.value()?;
                    self.trivia();
                    let comma = if self.peek() == Some(b',') {
                        self.pos += 1;
                        Some(self.pos - 1)
                    } else {
                        None
                    };
                    members.push(Member {
                        key,
                        key_start,
                        value,
                        comma,
                    });
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Node> {
        let start = self.pos;
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.trivia();
            if self.peek()? == b']' {
                self.pos += 1;
                return Some(Node {
                    start,
                    end: self.pos,
                    kind: NodeKind::Array(items),
                });
            }
            items.push(self.value()?);
            self.trivia();
            if self.peek() == Some(b',') {
                self.pos += 1;
            }
        }
    }
}
