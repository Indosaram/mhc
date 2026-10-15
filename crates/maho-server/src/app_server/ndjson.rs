use super::envelope::{ClassifiedIncoming, classify_incoming};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, PartialEq)]
pub enum NdjsonEmission {
    Incoming(ClassifiedIncoming),
    ParseError(Value),
}
pub fn parse_ndjson_line(line: &str) -> NdjsonEmission {
    match serde_json::from_str(line) {
        Ok(value) => NdjsonEmission::Incoming(classify_incoming(value)),
        Err(_) => NdjsonEmission::ParseError(
            json!({"id":null,"error":{"code":-32700,"message":"Parse error"}}),
        ),
    }
}
/// Borrowed outbound line; field order matches the wire shape built before.
#[derive(Serialize)]
struct OutboundLine<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
    #[serde(rename = "emittedAtMs", skip_serializing_if = "Option::is_none")]
    emitted_at_ms: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<OutboundError<'a>>,
}
#[derive(Serialize)]
struct OutboundError<'a> {
    code: &'a Value,
    message: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a Value>,
}
pub fn serialize_ndjson_message(message: &Value) -> Result<String, serde_json::Error> {
    let empty = OutboundLine {
        method: None,
        id: None,
        params: None,
        emitted_at_ms: None,
        result: None,
        error: None,
    };
    let output = if let Some(method) = message.get("method") {
        let id = message.get("id");
        OutboundLine {
            method: Some(method),
            id,
            params: message.get("params"),
            emitted_at_ms: if id.is_none() {
                message.get("emittedAtMs")
            } else {
                None
            },
            ..empty
        }
    } else if let Some(result) = message.get("result") {
        OutboundLine {
            id: Some(&message["id"]),
            result: Some(result),
            ..empty
        }
    } else {
        let error = &message["error"];
        OutboundLine {
            id: Some(&message["id"]),
            error: Some(OutboundError {
                code: &error["code"],
                message: &error["message"],
                data: error.get("data"),
            }),
            ..empty
        }
    };
    serde_json::to_string(&output).map(|mut text| {
        text.push('\n');
        text
    })
}

#[derive(Default)]
pub struct NdjsonReader {
    buffer: Vec<u8>,
}
impl NdjsonReader {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<NdjsonEmission> {
        let mut output = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                output.push(self.line());
            } else {
                self.buffer.push(*byte);
            }
        }
        output
    }
    fn line(&mut self) -> NdjsonEmission {
        let mut bytes = std::mem::take(&mut self.buffer);
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        parse_ndjson_line(&String::from_utf8_lossy(&bytes))
    }
    pub fn end(&mut self) -> Option<NdjsonEmission> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(self.line())
        }
    }
}
