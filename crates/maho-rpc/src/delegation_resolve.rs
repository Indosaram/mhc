//! Port of senpi `packages/coding-agent/src/modes/rpc/delegation-resolve.ts`.
//!
//! How a worker isolate turns the delegation descriptor a bind or command carries into the one it
//! reads (`omo-relay-daemon#307`). The worker keeps the FIRST buffer it saw for an incarnation
//! and compares scalars only, never wrapper identity.

use serde_json::{Map, Value};

use crate::connection_authority::{incarnation_of, read_authority};
use crate::delegation_declaration::same_principal;
use crate::delegation_record::{DelegationDescriptor, DelegationMode};

#[derive(Debug, Clone)]
pub enum DelegationResolution {
    /// Read `descriptor` (the retained buffer for its incarnation).
    Use { descriptor: DelegationDescriptor },
    /// An older incarnation than the retained one: the command runs, its role checks read retired.
    Stale { descriptor: DelegationDescriptor },
    /// A principal, mode or stamp that cannot belong to this connection: that one command is
    /// refused.
    Conflict,
}

#[derive(Debug, Clone)]
pub struct ResolveDelegationOutcome {
    pub resolution: DelegationResolution,
    pub retain: Option<DelegationDescriptor>,
}

pub const DELEGATION_AUTHORITY_CONFLICT: &str = "delegation_authority_conflict";

/// The one command's refusal when its carried delegation conflicts: non-fatal, no
/// `session_worker_` prefix.
pub fn authority_conflict(command: &Value) -> Value {
    let mut map = Map::new();
    if let Some(id) = command
        .as_object()
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
    {
        map.insert("id".to_owned(), Value::String(id.to_owned()));
    }
    let command_name = command
        .as_object()
        .and_then(|o| o.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("parse");
    map.insert("type".to_owned(), Value::String("response".to_owned()));
    map.insert("command".to_owned(), Value::String(command_name.to_owned()));
    map.insert("success".to_owned(), Value::Bool(false));
    map.insert(
        "error".to_owned(),
        Value::String(DELEGATION_AUTHORITY_CONFLICT.to_owned()),
    );
    map.insert(
        "errorCode".to_owned(),
        Value::String(DELEGATION_AUTHORITY_CONFLICT.to_owned()),
    );
    Value::Object(map)
}

pub fn resolve_delegation(
    retained: Option<&DelegationDescriptor>,
    wire: &DelegationDescriptor,
) -> ResolveDelegationOutcome {
    if let Some(retained) = retained
        && (retained.mode != wire.mode || !same_principal(&retained.principal, &wire.principal))
    {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Conflict,
            retain: None,
        };
    }
    if wire.mode == DelegationMode::Legacy {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Use {
                descriptor: retained.cloned().unwrap_or_else(|| wire.clone()),
            },
            retain: None,
        };
    }
    let (Some(incarnation), Some(wire_authority)) = (wire.incarnation, wire.authority.as_ref())
    else {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Conflict,
            retain: None,
        };
    };
    if incarnation_of(wire_authority) != incarnation {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Conflict,
            retain: None,
        };
    }
    let Some(retained) = retained else {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Use {
                descriptor: wire.clone(),
            },
            retain: Some(wire.clone()),
        };
    };
    let kept = retained.incarnation.unwrap_or(0);
    if incarnation == kept {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Use {
                descriptor: retained.clone(),
            },
            retain: None,
        };
    }
    if incarnation < kept {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Stale {
                descriptor: wire.clone(),
            },
            retain: None,
        };
    }
    // The host retires k before it issues k+1, so a newer incarnation over a live retained word
    // cannot be genuine.
    let retained_retired = retained
        .authority
        .as_ref()
        .is_some_and(|auth| read_authority(auth).retired);
    if !retained_retired {
        return ResolveDelegationOutcome {
            resolution: DelegationResolution::Conflict,
            retain: None,
        };
    }
    ResolveDelegationOutcome {
        resolution: DelegationResolution::Use {
            descriptor: wire.clone(),
        },
        retain: Some(wire.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_authority::{create_authority, mask_of_roles, store_retired};
    use crate::delegation_declaration::Principal;
    use serde_json::json;

    fn host_desc(incarnation: i32, stamp_incarnation: i32) -> DelegationDescriptor {
        DelegationDescriptor {
            principal: Principal {
                device_id: "dev".to_owned(),
                device_name: None,
                conn: None,
                same_account: None,
            },
            mode: DelegationMode::Host,
            incarnation: Some(incarnation),
            authority: Some(
                create_authority(stamp_incarnation, mask_of_roles(["interactive_user"]))
                    .expect("auth"),
            ),
        }
    }

    #[test]
    fn resolves_first_same_stale_and_rotated_incarnations() {
        let k1 = host_desc(1, 1);
        let first = resolve_delegation(None, &k1);
        assert!(matches!(first.resolution, DelegationResolution::Use { .. }));
        assert!(first.retain.is_some());

        let k2 = host_desc(2, 2);
        // Live k1 refuses k2 as conflict.
        assert!(matches!(
            resolve_delegation(Some(&k1), &k2).resolution,
            DelegationResolution::Conflict
        ));

        // Once k1 is retired, k2 is accepted and retained, and k1 is stale.
        store_retired(k1.authority.as_ref().expect("auth")).expect("retire");
        let rotated = resolve_delegation(Some(&k1), &k2);
        assert!(matches!(
            rotated.resolution,
            DelegationResolution::Use { .. }
        ));
        assert!(rotated.retain.is_some());

        let stale = resolve_delegation(Some(&k2), &k1);
        assert!(matches!(
            stale.resolution,
            DelegationResolution::Stale { .. }
        ));
    }

    #[test]
    fn rejects_mismatched_stamp_and_formats_conflict() {
        let bad = host_desc(2, 1);
        assert!(matches!(
            resolve_delegation(None, &bad).resolution,
            DelegationResolution::Conflict
        ));
        let err = authority_conflict(&json!({"id": "x", "type": "prompt"}));
        assert_eq!(err["id"], "x");
        assert_eq!(err["command"], "prompt");
        assert_eq!(err["errorCode"], DELEGATION_AUTHORITY_CONFLICT);
    }
}
