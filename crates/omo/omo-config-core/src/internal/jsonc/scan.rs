#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Comma,
    Colon,
    String,
    Number,
    True,
    False,
    Null,
    LineComment,
    BlockComment,
    Trivia,
    LineBreak,
    Unknown,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub offset: usize,
    pub length: usize,
}

impl Token {
    pub fn end(&self) -> usize {
        self.offset + self.length
    }
}

pub struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    error_count: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
            error_count: 0,
        }
    }

    pub fn error_count(&self) -> usize {
        self.error_count
    }

    pub fn text(&self) -> &'a str {
        self.text
    }

    pub fn scan(&mut self) -> Token {
        let len = self.bytes.len();
        if self.pos >= len {
            return Token {
                kind: TokenKind::Eof,
                offset: len,
                length: 0,
            };
        }
        let start = self.pos;
        let byte = self.bytes[start];
        let kind = match byte {
            b'{' => {
                self.pos += 1;
                TokenKind::OpenBrace
            }
            b'}' => {
                self.pos += 1;
                TokenKind::CloseBrace
            }
            b'[' => {
                self.pos += 1;
                TokenKind::OpenBracket
            }
            b']' => {
                self.pos += 1;
                TokenKind::CloseBracket
            }
            b',' => {
                self.pos += 1;
                TokenKind::Comma
            }
            b':' => {
                self.pos += 1;
                TokenKind::Colon
            }
            b' ' | b'\t' => {
                while self.pos < len && matches!(self.bytes[self.pos], b' ' | b'\t') {
                    self.pos += 1;
                }
                TokenKind::Trivia
            }
            b'\r' => {
                self.pos += if self.bytes.get(self.pos + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                TokenKind::LineBreak
            }
            b'\n' => {
                self.pos += 1;
                TokenKind::LineBreak
            }
            b'/' => self.scan_comment(),
            b'"' => self.scan_string(),
            b'-' | b'0'..=b'9' => self.scan_number(),
            b't' | b'f' | b'n' => self.scan_keyword(),
            _ => {
                self.pos += 1;
                self.error_count += 1;
                TokenKind::Unknown
            }
        };
        Token {
            kind,
            offset: start,
            length: self.pos - start,
        }
    }

    pub fn scan_significant(&mut self) -> Token {
        loop {
            let token = self.scan();
            if !matches!(
                token.kind,
                TokenKind::Trivia
                    | TokenKind::LineBreak
                    | TokenKind::LineComment
                    | TokenKind::BlockComment
            ) {
                return token;
            }
        }
    }

    fn scan_comment(&mut self) -> TokenKind {
        let len = self.bytes.len();
        match self.bytes.get(self.pos + 1) {
            Some(b'/') => {
                self.pos += 2;
                while self.pos < len && !matches!(self.bytes[self.pos], b'\r' | b'\n') {
                    self.pos += 1;
                }
                TokenKind::LineComment
            }
            Some(b'*') => {
                self.pos += 2;
                while self.pos < len {
                    if self.bytes[self.pos] == b'*' && self.bytes.get(self.pos + 1) == Some(&b'/') {
                        self.pos += 2;
                        return TokenKind::BlockComment;
                    }
                    self.pos += 1;
                }
                self.error_count += 1;
                TokenKind::BlockComment
            }
            _ => {
                self.pos += 1;
                self.error_count += 1;
                TokenKind::Unknown
            }
        }
    }

    fn scan_string(&mut self) -> TokenKind {
        let len = self.bytes.len();
        let mut index = self.pos + 1;
        let mut valid = true;
        loop {
            if index >= len {
                valid = false;
                break;
            }
            let byte = self.bytes[index];
            if byte == b'"' {
                index += 1;
                break;
            }
            if byte == b'\\' {
                let escape = self.bytes.get(index + 1).copied();
                match escape {
                    Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => index += 2,
                    Some(b'u') => {
                        let hex_start = index + 2;
                        let hex_end = hex_start + 4;
                        let complete = hex_end <= len
                            && self.bytes[hex_start..hex_end]
                                .iter()
                                .all(u8::is_ascii_hexdigit);
                        if complete {
                            index = hex_end;
                        } else {
                            valid = false;
                            index = hex_end.min(len);
                            break;
                        }
                    }
                    _ => {
                        valid = false;
                        break;
                    }
                }
                continue;
            }
            if byte < 0x20 {
                valid = false;
                break;
            }
            index += 1;
        }
        self.pos = index;
        if valid {
            TokenKind::String
        } else {
            self.error_count += 1;
            TokenKind::Unknown
        }
    }

    fn scan_number(&mut self) -> TokenKind {
        match scan_number_end(self.bytes, self.pos) {
            Some(end) => {
                self.pos = end;
                TokenKind::Number
            }
            None => {
                self.pos += 1;
                while self.pos < self.bytes.len()
                    && matches!(
                        self.bytes[self.pos],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    self.pos += 1;
                }
                self.error_count += 1;
                TokenKind::Unknown
            }
        }
    }

    fn scan_keyword(&mut self) -> TokenKind {
        let rest = &self.text[self.pos..];
        for (word, kind) in [
            ("true", TokenKind::True),
            ("false", TokenKind::False),
            ("null", TokenKind::Null),
        ] {
            if rest.starts_with(word) {
                self.pos += word.len();
                return kind;
            }
        }
        self.pos += 1;
        self.error_count += 1;
        TokenKind::Unknown
    }
}

pub fn is_eol(text: &str, offset: usize) -> bool {
    matches!(text.as_bytes().get(offset), Some(b'\r') | Some(b'\n'))
}

fn scan_number_end(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut index = start;
    if bytes.get(index) == Some(&b'-') {
        index += 1;
    }
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(byte) if byte.is_ascii_digit() => {
            while index < len && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
        _ => return None,
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        if index >= len || !bytes[index].is_ascii_digit() {
            return None;
        }
        while index < len && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        if index >= len || !bytes[index].is_ascii_digit() {
            return None;
        }
        while index < len && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    Some(index)
}
