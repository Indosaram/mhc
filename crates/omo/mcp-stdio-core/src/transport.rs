use std::io::Write;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioJsonRpcResponseMode {
    Line,
    Framed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StdioJsonRpcMessage {
    Request {
        payload: Value,
        response_mode: StdioJsonRpcResponseMode,
    },
    ParseError {
        message: String,
        response_mode: StdioJsonRpcResponseMode,
    },
}

impl StdioJsonRpcMessage {
    pub fn response_mode(&self) -> StdioJsonRpcResponseMode {
        match self {
            StdioJsonRpcMessage::Request { response_mode, .. }
            | StdioJsonRpcMessage::ParseError { response_mode, .. } => *response_mode,
        }
    }
}

const HEADER_SEPARATOR: &[u8] = b"\r\n\r\n";
const CONTENT_LENGTH_PREFIX: &[u8] = b"content-length:";

enum ReadStep {
    Incomplete,
    Complete {
        message: Option<StdioJsonRpcMessage>,
        consumed: usize,
    },
}

#[derive(Debug, Default)]
pub struct StdioJsonRpcDecoder {
    buffer: Vec<u8>,
}

impl StdioJsonRpcDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<StdioJsonRpcMessage> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            match read_next(&self.buffer) {
                ReadStep::Incomplete => break,
                ReadStep::Complete { message, consumed } => {
                    self.buffer.drain(..consumed);
                    if let Some(message) = message {
                        messages.push(message);
                    }
                }
            }
        }
        messages
    }

    pub fn finish(&mut self) -> Option<StdioJsonRpcMessage> {
        let trailing = String::from_utf8_lossy(&self.buffer).trim().to_string();
        self.buffer.clear();
        if trailing.is_empty() {
            return None;
        }
        Some(parse_json_payload(
            &trailing,
            StdioJsonRpcResponseMode::Line,
        ))
    }
}

pub fn decode_stdio_json_rpc_messages(chunks: &[u8]) -> Vec<StdioJsonRpcMessage> {
    let mut decoder = StdioJsonRpcDecoder::new();
    let mut messages = decoder.push(chunks);
    if let Some(trailing) = decoder.finish() {
        messages.push(trailing);
    }
    messages
}

pub fn write_stdio_json_rpc_response<W, R>(
    output: &mut W,
    response: &R,
    response_mode: StdioJsonRpcResponseMode,
) -> std::io::Result<()>
where
    W: Write,
    R: Serialize + ?Sized,
{
    let body = serde_json::to_string(response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let payload = match response_mode {
        StdioJsonRpcResponseMode::Framed => {
            format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
        }
        StdioJsonRpcResponseMode::Line => format!("{body}\n"),
    };
    output.write_all(payload.as_bytes())
}

fn read_next(buffer: &[u8]) -> ReadStep {
    if buffer.is_empty() {
        return ReadStep::Incomplete;
    }
    if starts_with_content_length(buffer) {
        read_framed_message(buffer)
    } else {
        read_line_message(buffer)
    }
}

fn read_line_message(buffer: &[u8]) -> ReadStep {
    let Some(newline_index) = buffer.iter().position(|byte| *byte == b'\n') else {
        return ReadStep::Incomplete;
    };
    let consumed = newline_index + 1;
    let raw = &buffer[..newline_index];
    let trimmed_carriage_return = raw.strip_suffix(b"\r").unwrap_or(raw);
    let line = String::from_utf8_lossy(trimmed_carriage_return).to_string();
    if line.trim().is_empty() {
        return ReadStep::Complete {
            message: None,
            consumed,
        };
    }
    ReadStep::Complete {
        message: Some(parse_json_payload(&line, StdioJsonRpcResponseMode::Line)),
        consumed,
    }
}

fn read_framed_message(buffer: &[u8]) -> ReadStep {
    let Some(separator_index) = find_separator(buffer) else {
        return ReadStep::Incomplete;
    };
    let headers = String::from_utf8_lossy(&buffer[..separator_index]).to_string();
    let body_start = separator_index + HEADER_SEPARATOR.len();
    let Some(content_length) = parse_content_length(&headers) else {
        return ReadStep::Complete {
            message: Some(StdioJsonRpcMessage::ParseError {
                message: "Missing or invalid Content-Length header".to_string(),
                response_mode: StdioJsonRpcResponseMode::Framed,
            }),
            consumed: body_start,
        };
    };
    let body_end = body_start + content_length;
    if buffer.len() < body_end {
        return ReadStep::Incomplete;
    }
    let body = String::from_utf8_lossy(&buffer[body_start..body_end]).to_string();
    ReadStep::Complete {
        message: Some(parse_json_payload(&body, StdioJsonRpcResponseMode::Framed)),
        consumed: body_end,
    }
}

fn find_separator(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(HEADER_SEPARATOR.len())
        .position(|window| window == HEADER_SEPARATOR)
}

fn starts_with_content_length(buffer: &[u8]) -> bool {
    let take = CONTENT_LENGTH_PREFIX.len().min(buffer.len());
    buffer[..take].eq_ignore_ascii_case(CONTENT_LENGTH_PREFIX)
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        return value.parse::<usize>().ok();
    }
    None
}

fn parse_json_payload(
    payload: &str,
    response_mode: StdioJsonRpcResponseMode,
) -> StdioJsonRpcMessage {
    match serde_json::from_str::<Value>(payload) {
        Ok(value) => StdioJsonRpcMessage::Request {
            payload: value,
            response_mode,
        },
        Err(error) => StdioJsonRpcMessage::ParseError {
            message: error.to_string(),
            response_mode,
        },
    }
}
