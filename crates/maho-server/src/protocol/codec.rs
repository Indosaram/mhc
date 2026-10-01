use super::{
    cbor::{CborOptions, CborValue, decode_cbor, encode_cbor},
    framing::{DEFAULT_MAX_FRAME_LENGTH, FrameDecoder, encode_frame},
    messages::{PROTOCOL_VERSION, valid_client_message, valid_server_message},
};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ProtocolValidationError(pub String);

#[derive(Debug, Clone, Copy)]
pub enum MessageKind {
    Client,
    Server,
}

impl MessageKind {
    fn name(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
    fn parse(self, value: &Value) -> Result<(), ProtocolValidationError> {
        let valid = match self {
            Self::Client => valid_client_message(value),
            Self::Server => valid_server_message(value),
        };
        if valid {
            Ok(())
        } else {
            Err(ProtocolValidationError(format!(
                "Invalid {} protocol message",
                self.name()
            )))
        }
    }
}

pub fn parse_client_message(value: &Value) -> Result<&Value, ProtocolValidationError> {
    MessageKind::Client.parse(value)?;
    Ok(value)
}
pub fn parse_server_message(value: &Value) -> Result<&Value, ProtocolValidationError> {
    MessageKind::Server.parse(value)?;
    Ok(value)
}

fn bounded(message: &str) -> String {
    if message.chars().count() <= 500 {
        message.into()
    } else {
        format!("{}...", message.chars().take(497).collect::<String>())
    }
}

fn encode(value: &Value, kind: MessageKind, max: u32) -> Result<Vec<u8>, ProtocolValidationError> {
    kind.parse(value)?;
    let options = CborOptions {
        max_byte_length: usize::try_from(max)
            .map_err(|e| ProtocolValidationError(e.to_string()))?,
        ..CborOptions::default()
    };
    let cbor = CborValue::from_json(value)
        .and_then(|v| encode_cbor(&v, options))
        .map_err(|e| {
            ProtocolValidationError(format!(
                "Unable to encode {} protocol message: {}",
                kind.name(),
                bounded(&e.to_string())
            ))
        })?;
    encode_frame(&cbor).map_err(|e| {
        ProtocolValidationError(format!(
            "Unable to encode {} protocol message: {}",
            kind.name(),
            bounded(&e.to_string())
        ))
    })
}
pub fn encode_client_message(value: &Value, max: u32) -> Result<Vec<u8>, ProtocolValidationError> {
    encode(value, MessageKind::Client, max)
}
pub fn encode_server_message(value: &Value, max: u32) -> Result<Vec<u8>, ProtocolValidationError> {
    encode(value, MessageKind::Server, max)
}
pub fn is_supported_protocol_version(version: u64) -> bool {
    version == PROTOCOL_VERSION
}

pub struct MessageDecoder {
    failed: bool,
    frames: FrameDecoder,
    kind: MessageKind,
    max: u32,
}

impl MessageDecoder {
    pub fn new(kind: MessageKind, max: u32) -> Self {
        Self {
            failed: false,
            frames: FrameDecoder::new(max),
            kind,
            max,
        }
    }
    pub fn client() -> Self {
        Self::new(MessageKind::Client, DEFAULT_MAX_FRAME_LENGTH)
    }
    pub fn server() -> Self {
        Self::new(MessageKind::Server, DEFAULT_MAX_FRAME_LENGTH)
    }
    fn check(&self) -> Result<(), ProtocolValidationError> {
        if self.failed {
            Err(ProtocolValidationError(format!(
                "{} message decoder has failed",
                self.kind.name()
            )))
        } else {
            Ok(())
        }
    }
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, ProtocolValidationError> {
        self.check()?;
        let result = (|| {
            let frames = self.frames.push(chunk).map_err(|e| {
                ProtocolValidationError(format!(
                    "Invalid {} protocol frame: {}",
                    self.kind.name(),
                    bounded(&e.to_string())
                ))
            })?;
            let mut messages = Vec::new();
            for frame in frames {
                let options = CborOptions {
                    max_byte_length: usize::try_from(self.max)
                        .map_err(|e| ProtocolValidationError(e.to_string()))?,
                    ..CborOptions::default()
                };
                let value = decode_cbor(&frame, options)
                    .and_then(CborValue::into_json)
                    .map_err(|e| {
                        ProtocolValidationError(format!(
                            "Invalid {} protocol frame: {}",
                            self.kind.name(),
                            bounded(&e.to_string())
                        ))
                    })?;
                self.kind.parse(&value)?;
                messages.push(value);
            }
            Ok(messages)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn end(&mut self) -> Result<(), ProtocolValidationError> {
        self.check()?;
        self.frames.end().map_err(|e| {
            self.failed = true;
            ProtocolValidationError(format!(
                "Invalid {} protocol framing: {}",
                self.kind.name(),
                bounded(&e.to_string())
            ))
        })
    }
}
