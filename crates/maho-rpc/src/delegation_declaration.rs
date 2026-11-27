//! Port of senpi `packages/coding-agent/src/modes/rpc/delegation-declaration.ts`.
//!
//! The `declare_delegation` record (`senpi#5632`): its principal and granted classes, parsed at the
//! boundary.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::method_classes::RpcMethodClass;

const MAX_PRINCIPAL_FIELD: usize = 256;
const MAX_CONN_FIELD: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conn: Option<String>,
    /// The forwarder states this device is the host's own account (`senpi#6342`); fixed for the
    /// record's life.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_account: Option<bool>,
}

/// The connection roles a host-mode declaration may carry (`omo-relay-daemon#307`).
const DECLARABLE_ROLES: [&str; 3] = ["interactive_user", "task_parent", "peer_agent"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub principal: Principal,
    pub granted: HashSet<RpcMethodClass>,
    /// Present: host mode (`delegation_roles_v1`), the role ceiling the forwarder allows.
    /// Absent: legacy mode.
    pub roles: Option<HashSet<String>>,
}

fn bounded_string(value: Option<&Value>, max: usize) -> Option<&str> {
    let s = value?.as_str()?;
    (1..=max).contains(&s.len()).then_some(s)
}

fn parse_forwardable_class(entry: &str) -> Option<RpcMethodClass> {
    match entry {
        "observe" => Some(RpcMethodClass::Observe),
        "drive" => Some(RpcMethodClass::Drive),
        "answer" => Some(RpcMethodClass::Answer),
        "thread" => Some(RpcMethodClass::Thread),
        "device_admin" => Some(RpcMethodClass::DeviceAdmin),
        _ => None,
    }
}

fn parse_roles(value: Option<&Value>) -> Result<Option<HashSet<String>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let invalid = "roles holds each of interactive_user, task_parent, peer_agent at most once";
    let Some(arr) = value.as_array() else {
        return Err(invalid.to_owned());
    };
    let mut roles = HashSet::new();
    for entry in arr {
        let Some(role) = entry.as_str() else {
            return Err(invalid.to_owned());
        };
        if !DECLARABLE_ROLES.contains(&role) || !roles.insert(role.to_owned()) {
            return Err(invalid.to_owned());
        }
    }
    Ok(Some(roles))
}

pub fn parse_declaration(record: &Value) -> Result<Declaration, String> {
    let Some(obj) = record.as_object() else {
        return Err("principal must be an object".to_owned());
    };
    let Some(principal) = obj.get("principal").and_then(Value::as_object) else {
        return Err("principal must be an object".to_owned());
    };
    let Some(device_id) = bounded_string(principal.get("deviceId"), MAX_PRINCIPAL_FIELD) else {
        return Err("principal.deviceId must be 1-256 characters".to_owned());
    };
    let device_name = match principal.get("deviceName") {
        None => None,
        some => Some(
            bounded_string(some, MAX_PRINCIPAL_FIELD)
                .ok_or_else(|| "principal.deviceName must be 1-256 characters".to_owned())?
                .to_owned(),
        ),
    };
    let conn = match principal.get("conn") {
        None => None,
        some => Some(
            bounded_string(some, MAX_CONN_FIELD)
                .ok_or_else(|| "principal.conn must be 1-64 characters".to_owned())?
                .to_owned(),
        ),
    };
    let same_account = match principal.get("sameAccount") {
        None => None,
        Some(Value::Bool(true)) => Some(true),
        Some(_) => return Err("principal.sameAccount must be true when present".to_owned()),
    };

    let Some(granted_arr) = obj.get("granted").and_then(Value::as_array) else {
        return Err("granted must be an array of method classes".to_owned());
    };
    let mut classes = HashSet::new();
    for entry in granted_arr {
        let Some(name) = entry.as_str() else {
            return Err(
                "granted holds each of observe, drive, answer, thread, device_admin at most once"
                    .to_owned(),
            );
        };
        let Some(class) = parse_forwardable_class(name) else {
            return Err(
                "granted holds each of observe, drive, answer, thread, device_admin at most once"
                    .to_owned(),
            );
        };
        if !classes.insert(class) {
            return Err(
                "granted holds each of observe, drive, answer, thread, device_admin at most once"
                    .to_owned(),
            );
        }
    }

    let roles = parse_roles(obj.get("roles"))?;
    Ok(Declaration {
        principal: Principal {
            device_id: device_id.to_owned(),
            device_name,
            conn,
            same_account,
        },
        granted: classes,
        roles,
    })
}

pub fn same_principal(a: &Principal, b: &Principal) -> bool {
    a.device_id == b.device_id
        && a.device_name == b.device_name
        && a.conn == b.conn
        && a.same_account == b.same_account
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_legacy_and_host_declarations() {
        let legacy = parse_declaration(&json!({
            "principal": { "deviceId": "d1", "sameAccount": true },
            "granted": ["observe", "drive"]
        }))
        .expect("legacy");
        assert_eq!(legacy.principal.device_id, "d1");
        assert_eq!(legacy.principal.same_account, Some(true));
        assert!(legacy.roles.is_none());
        assert!(legacy.granted.contains(&RpcMethodClass::Observe));
        assert!(legacy.granted.contains(&RpcMethodClass::Drive));

        let host = parse_declaration(&json!({
            "principal": { "deviceId": "d1", "deviceName": "Mac", "conn": "c1" },
            "granted": ["observe"],
            "roles": ["interactive_user", "peer_agent"]
        }))
        .expect("host");
        assert_eq!(
            host.roles.expect("roles"),
            HashSet::from(["interactive_user".to_owned(), "peer_agent".to_owned()])
        );
    }

    #[test]
    fn rejects_duplicates_invalid_classes_roles_and_same_account_false() {
        for bad in [
            json!({ "principal": null, "granted": [] }),
            json!({ "principal": { "deviceId": "" }, "granted": [] }),
            json!({ "principal": { "deviceId": "d", "sameAccount": false }, "granted": [] }),
            json!({ "principal": { "deviceId": "d" }, "granted": ["connection_local"] }),
            json!({ "principal": { "deviceId": "d" }, "granted": ["observe", "observe"] }),
            json!({ "principal": { "deviceId": "d" }, "granted": [], "roles": ["unknown"] }),
            json!({ "principal": { "deviceId": "d" }, "granted": [], "roles": ["peer_agent", "peer_agent"] }),
        ] {
            assert!(parse_declaration(&bad).is_err(), "{bad}");
        }
    }
}
