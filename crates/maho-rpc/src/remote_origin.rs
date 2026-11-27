//! Port of senpi `packages/coding-agent/src/modes/rpc/remote-origin.ts`.
//!
//! The remote device a connection speaks for (`senpi#5475`, `omo-relay-daemon#184` P5).
//! A relay forwarder opens one engine connection per remote sender and declares, through its own
//! `set_client_info`, which device that connection speaks for. Every dialog a command of that
//! connection raises then carries the value as `remoteOrigin`, so the host's desktop and TUI can
//! label it `"Requested from <device>"`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_DEVICE_ID_CHARACTERS: usize = 256;
const MAX_DEVICE_NAME_CHARACTERS: usize = 256;

pub const REMOTE_ORIGIN_INVALID: &str = "remote_origin_invalid: remoteOrigin must be { deviceId: non-empty string, deviceName?: non-empty string }";
pub const REMOTE_ORIGIN_FIXED: &str =
    "remote_origin_fixed: a connection's remoteOrigin cannot change after it is declared";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcRemoteOrigin {
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOriginDeclaration {
    Valid(Option<RpcRemoteOrigin>),
    Refusal(&'static str),
}

fn bounded_text(value: Option<&Value>, max: usize) -> Option<&str> {
    let text = value?.as_str()?;
    (!text.is_empty() && text.len() <= max).then_some(text)
}

/// The declared origin, `Ok(None)` when the value is absent, or `Err(())` for any invalid shape.
fn parse_remote_origin(value: Option<&Value>) -> Result<Option<RpcRemoteOrigin>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Err(());
    };
    if object
        .keys()
        .any(|key| key != "deviceId" && key != "deviceName")
    {
        return Err(());
    }
    let Some(device_id) = bounded_text(object.get("deviceId"), MAX_DEVICE_ID_CHARACTERS) else {
        return Err(());
    };
    let device_name = match object.get("deviceName") {
        None => None,
        some => {
            let Some(name) = bounded_text(some, MAX_DEVICE_NAME_CHARACTERS) else {
                return Err(());
            };
            Some(name.to_owned())
        }
    };
    Ok(Some(RpcRemoteOrigin {
        device_id: device_id.to_owned(),
        device_name,
    }))
}

/// Validates a `set_client_info.remoteOrigin` against what the connection already declared, before
/// anything of the declaration is committed: the origin to record (`None` when absent), or the
/// refusal to answer.
pub fn remote_origin_declaration(
    value: Option<&Value>,
    accepts: impl FnOnce(&RpcRemoteOrigin) -> bool,
) -> RemoteOriginDeclaration {
    let Ok(origin) = parse_remote_origin(value) else {
        return RemoteOriginDeclaration::Refusal(REMOTE_ORIGIN_INVALID);
    };
    if let Some(ref origin) = origin
        && !accepts(origin)
    {
        return RemoteOriginDeclaration::Refusal(REMOTE_ORIGIN_FIXED);
    }
    RemoteOriginDeclaration::Valid(origin)
}

pub fn same_remote_origin(a: &RpcRemoteOrigin, b: &RpcRemoteOrigin) -> bool {
    a.device_id == b.device_id && a.device_name == b.device_name
}

pub fn remote_origin_field(origin: Option<&RpcRemoteOrigin>) -> Map<String, Value> {
    let mut map = Map::new();
    if let Some(origin) = origin
        && let Ok(val) = serde_json::to_value(origin)
    {
        map.insert("remoteOrigin".to_owned(), val);
    }
    map
}

pub fn origin_field(origin: Option<&RpcRemoteOrigin>) -> Map<String, Value> {
    let mut map = Map::new();
    if let Some(origin) = origin
        && let Ok(val) = serde_json::to_value(origin)
    {
        map.insert("origin".to_owned(), val);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_absent_and_valid_origins() {
        assert_eq!(
            remote_origin_declaration(None, |_| true),
            RemoteOriginDeclaration::Valid(None)
        );
        let val = json!({"deviceId": "dev-1", "deviceName": "Laptop"});
        assert_eq!(
            remote_origin_declaration(Some(&val), |_| true),
            RemoteOriginDeclaration::Valid(Some(RpcRemoteOrigin {
                device_id: "dev-1".to_owned(),
                device_name: Some("Laptop".to_owned()),
            }))
        );
    }

    #[test]
    fn rejects_invalid_shapes_and_conflicting_redeclarations() {
        for bad in [
            json!(null),
            json!("dev-1"),
            json!([]),
            json!({}),
            json!({"deviceId": ""}),
            json!({"deviceId": "x", "deviceName": ""}),
            json!({"deviceId": "x", "extra": true}),
            json!({"deviceId": "a".repeat(257)}),
        ] {
            assert_eq!(
                remote_origin_declaration(Some(&bad), |_| true),
                RemoteOriginDeclaration::Refusal(REMOTE_ORIGIN_INVALID)
            );
        }

        let val = json!({"deviceId": "dev-2"});
        assert_eq!(
            remote_origin_declaration(Some(&val), |_| false),
            RemoteOriginDeclaration::Refusal(REMOTE_ORIGIN_FIXED)
        );
    }

    #[test]
    fn field_helpers_omit_when_absent_and_match_equality() {
        assert!(remote_origin_field(None).is_empty());
        assert!(origin_field(None).is_empty());
        let a = RpcRemoteOrigin {
            device_id: "d1".to_owned(),
            device_name: Some("Phone".to_owned()),
        };
        let b = RpcRemoteOrigin {
            device_id: "d1".to_owned(),
            device_name: Some("Phone".to_owned()),
        };
        let c = RpcRemoteOrigin {
            device_id: "d1".to_owned(),
            device_name: None,
        };
        assert!(same_remote_origin(&a, &b));
        assert!(!same_remote_origin(&a, &c));
        assert_eq!(
            remote_origin_field(Some(&a)).get("remoteOrigin"),
            Some(&json!({"deviceId": "d1", "deviceName": "Phone"}))
        );
        assert_eq!(
            origin_field(Some(&c)).get("origin"),
            Some(&json!({"deviceId": "d1"}))
        );
    }
}
