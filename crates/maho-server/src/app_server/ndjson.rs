use super::envelope::{ClassifiedIncoming, classify_incoming};
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
pub fn serialize_ndjson_message(message: &Value) -> Result<String, serde_json::Error> {
    let output = if message.get("method").is_some() {
        let mut output = json!({"method":message["method"]});
        if let Some(id) = message.get("id") {
            output["id"] = id.clone();
        }
        if let Some(params) = message.get("params") {
            output["params"] = params.clone();
        }
        if message.get("id").is_none()
            && let Some(timestamp) = message.get("emittedAtMs")
        {
            output["emittedAtMs"] = timestamp.clone();
        }
        output
    } else if let Some(result) = message.get("result") {
        json!({"id":message["id"],"result":result})
    } else {
        let mut error =
            json!({"code":message["error"]["code"],"message":message["error"]["message"]});
        if let Some(data) = message["error"].get("data") {
            error["data"] = data.clone();
        }
        json!({"id":message["id"],"error":error})
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
