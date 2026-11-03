//! Port of `desktop-sync/protocol.ts`: the private wire schema. The zod schemas are `.strict()`, so
//! every parser here rejects unknown keys and wrong types.

use std::fmt;

use serde_json::{Map, Value};

const MAX_TOKEN_CHARS: usize = 256;
const MAX_ID: u64 = 0x7fff_ffff;

/// `DesktopSyncUnavailable`: the channel is unavailable or retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopSyncUnavailable;

impl fmt::Display for DesktopSyncUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The private desktop sync channel is unavailable or retired.")
    }
}

impl std::error::Error for DesktopSyncUnavailable {}

/// A typed operation the engine may send over the channel.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopSyncRequest {
    Descriptor,
    OperationBegin,
    Capability,
    OperationCancel { handle: String },
    OperationEnd { handle: String },
    Service { handle: String, data: Value },
    Keyring { handle: String, data: Value },
    GenesisRun { handle: String, data: Value },
}

/// An unsolicited event from the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopSyncEvent {
    Retired {
        child: String,
    },
    Advanced {
        child: String,
        handle: String,
        head: u64,
    },
    Connected {
        child: String,
        handle: String,
        connected: bool,
    },
}

impl DesktopSyncEvent {
    pub fn child(&self) -> &str {
        match self {
            Self::Retired { child }
            | Self::Advanced { child, .. }
            | Self::Connected { child, .. } => child,
        }
    }
}

/// A reply to one request id.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopSyncReply {
    Ok { id: u32, value: Value },
    Refused { id: u32 },
}

fn token(object: &Map<String, Value>, key: &str) -> Result<String, DesktopSyncUnavailable> {
    match object.get(key) {
        Some(Value::String(text))
            if !text.is_empty() && text.chars().count() <= MAX_TOKEN_CHARS =>
        {
            Ok(text.clone())
        }
        _ => Err(DesktopSyncUnavailable),
    }
}

fn exact_keys(object: &Map<String, Value>, keys: &[&str]) -> Result<(), DesktopSyncUnavailable> {
    if object.keys().all(|key| keys.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(DesktopSyncUnavailable)
    }
}

impl DesktopSyncRequest {
    /// `desktopSyncRequest.safeParse`.
    pub fn parse(value: &Value) -> Result<Self, DesktopSyncUnavailable> {
        let object = value.as_object().ok_or(DesktopSyncUnavailable)?;
        let op = object
            .get("op")
            .and_then(Value::as_str)
            .ok_or(DesktopSyncUnavailable)?;
        match op {
            "descriptor" | "operationBegin" | "capability" => {
                exact_keys(object, &["op"])?;
                Ok(match op {
                    "descriptor" => Self::Descriptor,
                    "operationBegin" => Self::OperationBegin,
                    _ => Self::Capability,
                })
            }
            "operationCancel" | "operationEnd" => {
                exact_keys(object, &["op", "handle"])?;
                let handle = token(object, "handle")?;
                Ok(if op == "operationCancel" {
                    Self::OperationCancel { handle }
                } else {
                    Self::OperationEnd { handle }
                })
            }
            "service" | "keyring" | "genesisRun" => {
                exact_keys(object, &["op", "handle", "data"])?;
                let handle = token(object, "handle")?;
                let data = object.get("data").cloned().unwrap_or(Value::Null);
                Ok(match op {
                    "service" => Self::Service { handle, data },
                    "keyring" => Self::Keyring { handle, data },
                    _ => Self::GenesisRun { handle, data },
                })
            }
            _ => Err(DesktopSyncUnavailable),
        }
    }

    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        let (op, handle, data) = match self {
            Self::Descriptor => ("descriptor", None, None),
            Self::OperationBegin => ("operationBegin", None, None),
            Self::Capability => ("capability", None, None),
            Self::OperationCancel { handle } => ("operationCancel", Some(handle), None),
            Self::OperationEnd { handle } => ("operationEnd", Some(handle), None),
            Self::Service { handle, data } => ("service", Some(handle), Some(data)),
            Self::Keyring { handle, data } => ("keyring", Some(handle), Some(data)),
            Self::GenesisRun { handle, data } => ("genesisRun", Some(handle), Some(data)),
        };
        object.insert("op".into(), op.into());
        if let Some(handle) = handle {
            object.insert("handle".into(), handle.clone().into());
        }
        if let Some(data) = data {
            object.insert("data".into(), data.clone());
        }
        Value::Object(object)
    }
}

impl DesktopSyncEvent {
    /// `desktopSyncEvent.parse` for a packet that carries an `event` key.
    pub fn parse(value: &Value) -> Result<Self, DesktopSyncUnavailable> {
        let object = value.as_object().ok_or(DesktopSyncUnavailable)?;
        match object.get("event").and_then(Value::as_str) {
            Some("retired") => {
                exact_keys(object, &["event", "child"])?;
                Ok(Self::Retired {
                    child: token(object, "child")?,
                })
            }
            Some("advanced") => {
                exact_keys(object, &["event", "child", "handle", "head"])?;
                let head = object
                    .get("head")
                    .and_then(Value::as_u64)
                    .ok_or(DesktopSyncUnavailable)?;
                Ok(Self::Advanced {
                    child: token(object, "child")?,
                    handle: token(object, "handle")?,
                    head,
                })
            }
            Some("connected") => {
                exact_keys(object, &["event", "child", "handle", "connected"])?;
                let connected = object
                    .get("connected")
                    .and_then(Value::as_bool)
                    .ok_or(DesktopSyncUnavailable)?;
                Ok(Self::Connected {
                    child: token(object, "child")?,
                    handle: token(object, "handle")?,
                    connected,
                })
            }
            _ => Err(DesktopSyncUnavailable),
        }
    }
}

impl DesktopSyncReply {
    /// `desktopSyncReply.parse`.
    pub fn parse(value: &Value) -> Result<Self, DesktopSyncUnavailable> {
        let object = value.as_object().ok_or(DesktopSyncUnavailable)?;
        let id = object
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| (1..=MAX_ID).contains(id))
            .ok_or(DesktopSyncUnavailable)? as u32;
        match object.get("ok").and_then(Value::as_bool) {
            Some(true) => {
                exact_keys(object, &["id", "ok", "value"])?;
                Ok(Self::Ok {
                    id,
                    value: object.get("value").cloned().unwrap_or(Value::Null),
                })
            }
            Some(false) => {
                exact_keys(object, &["id", "ok", "reason"])?;
                if object.get("reason").and_then(Value::as_str) == Some("refused") {
                    Ok(Self::Refused { id })
                } else {
                    Err(DesktopSyncUnavailable)
                }
            }
            None => Err(DesktopSyncUnavailable),
        }
    }
}

/// `desktopSyncChild.parse`: `{ child }` and nothing else.
pub fn parse_child(value: &Value) -> Result<String, DesktopSyncUnavailable> {
    let object = value.as_object().ok_or(DesktopSyncUnavailable)?;
    exact_keys(object, &["child"])?;
    token(object, "child")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn requests_are_strict() {
        assert_eq!(
            DesktopSyncRequest::parse(&json!({"op": "operationBegin"})),
            Ok(DesktopSyncRequest::OperationBegin)
        );
        assert!(DesktopSyncRequest::parse(&json!({"op": "operationBegin", "extra": 1})).is_err());
        assert!(DesktopSyncRequest::parse(&json!({"op": "operationEnd"})).is_err());
        assert!(DesktopSyncRequest::parse(&json!({"op": "operationEnd", "handle": ""})).is_err());
        assert!(
            DesktopSyncRequest::parse(&json!({"op": "operationEnd", "handle": "x".repeat(257)}))
                .is_err()
        );
        let service = json!({"op": "service", "handle": "h", "data": {"a": 1}});
        assert_eq!(
            DesktopSyncRequest::parse(&service).unwrap().to_value(),
            service
        );
        assert!(DesktopSyncRequest::parse(&json!({"op": "bootstrap"})).is_err());
    }

    #[test]
    fn events_and_replies_are_strict() {
        assert_eq!(
            DesktopSyncEvent::parse(
                &json!({"event": "advanced", "child": "c", "handle": "h", "head": 3})
            ),
            Ok(DesktopSyncEvent::Advanced {
                child: "c".into(),
                handle: "h".into(),
                head: 3
            })
        );
        assert!(
            DesktopSyncEvent::parse(
                &json!({"event": "advanced", "child": "c", "handle": "h", "head": -1})
            )
            .is_err()
        );
        assert!(
            DesktopSyncEvent::parse(&json!({"event": "retired", "child": "c", "x": 1})).is_err()
        );
        assert_eq!(
            DesktopSyncReply::parse(&json!({"id": 1, "ok": true, "value": 5})),
            Ok(DesktopSyncReply::Ok {
                id: 1,
                value: json!(5)
            })
        );
        assert_eq!(
            DesktopSyncReply::parse(&json!({"id": 2, "ok": false, "reason": "refused"})),
            Ok(DesktopSyncReply::Refused { id: 2 })
        );
        assert!(DesktopSyncReply::parse(&json!({"id": 0, "ok": true, "value": 1})).is_err());
        assert!(
            DesktopSyncReply::parse(&json!({"id": 1, "ok": false, "reason": "other"})).is_err()
        );
        assert_eq!(parse_child(&json!({"child": "c"})), Ok("c".to_string()));
        assert!(parse_child(&json!({"child": "c", "x": 1})).is_err());
    }
}
