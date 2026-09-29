//! Port of `socket-jsonrpc.ts`: newline-delimited JSON framing for the daemon socket.

use serde_json::Value;

/// TS `encodeJsonLine`.
pub fn encode_json_line(message: &Value) -> String {
    format!("{message}\n")
}

/// One decoded line: the JSON value, or the raw text and its parse error.
pub type DecodedLine = Result<Value, (String, serde_json::Error)>;

/// Callback-free core of the line decoder, usable across `await` points.
#[derive(Debug, Default)]
pub struct LineBuffer {
    buffer: Vec<u8>,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `chunk` and returns every complete, non-blank line decoded in order.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<DecodedLine> {
        self.buffer.extend_from_slice(chunk);
        let mut decoded = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=index).collect();
            let raw = String::from_utf8_lossy(&line[..index]);
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            decoded
                .push(serde_json::from_str::<Value>(raw).map_err(|error| (raw.to_string(), error)));
        }
        decoded
    }
}

/// TS `createLineDecoder`: buffers chunks and emits one callback per complete line.
pub struct LineDecoder<M, P>
where
    M: FnMut(Value),
    P: FnMut(&str, &serde_json::Error),
{
    buffer: LineBuffer,
    on_message: M,
    on_parse_error: P,
}

impl<M, P> LineDecoder<M, P>
where
    M: FnMut(Value),
    P: FnMut(&str, &serde_json::Error),
{
    pub fn new(on_message: M, on_parse_error: P) -> Self {
        Self {
            buffer: LineBuffer::new(),
            on_message,
            on_parse_error,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        for line in self.buffer.push(chunk) {
            match line {
                Ok(value) => (self.on_message)(value),
                Err((raw, error)) => (self.on_parse_error)(&raw, &error),
            }
        }
    }
}

#[cfg(test)]
#[path = "socket_jsonrpc_tests.rs"]
mod tests;
