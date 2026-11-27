//! Port of senpi `packages/coding-agent/src/modes/rpc/delegated-connection.ts`.
//!
//! Delegated connections (`senpi#5632`): the engine enforces its own method-class table on a
//! connection that speaks for someone else, so a forwarder (a relay host forwarding a remote
//! viewer's records) keeps no table of its own.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::connection_authority::INCARNATION_MAX;
use crate::delegated_presenter_records::{
    PRESENTER_CONTROL_INFO_COMMAND, is_private_presenter_type, private_presenter_verdict,
};
use crate::delegation_classify::{CLASSIFY_RECORD_COMMAND, classify_record};
use crate::delegation_declaration::parse_declaration;
use crate::delegation_record::{
    DelegationDescriptor, DelegationRecord, close_record, create_record, descriptor_of,
    echoed_roles, narrow_record, rotate_incarnation,
};
use crate::delegation_wire::{
    DelegationRefusal, DelegationResponse, DelegationVerdict, is_machine_op_command,
    machine_op_refusal, refused, request_id,
};
use crate::method_classes::{
    RPC_METHOD_CLASS_ENTRIES, RpcMethodClass, RpcMethodClassEntry, classify_rpc_method,
    rpc_method_classes_digest_value,
};

pub const DECLARE_DELEGATION_COMMAND: &str = "declare_delegation";
/// The record a public-key filter hands the host core, in band and in order, once its client
/// proved the launch key (`senpi#5632`).
pub const RPC_LAUNCH_PROOF_RECORD: &str = "launch_proven";

/// How a connection's transport proves the launch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchProofSource {
    Proven,
    InBand,
}

/// What the roles need of a delegated connection: its principal, mode and current incarnation.
#[derive(Debug, Clone)]
pub struct DelegatedConnectionView {
    pub descriptor: DelegationDescriptor,
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionLocalRequirement {
    SelfOnly,
    Grant(RpcMethodClass),
}

/// What a delegated connection needs for each `connection_local` method (DS2b section 2c).
fn connection_local_requirement(command: &str) -> Option<ConnectionLocalRequirement> {
    match command {
        "set_client_info" => Some(ConnectionLocalRequirement::SelfOnly),
        "subscribe_sessions" => Some(ConnectionLocalRequirement::Grant(RpcMethodClass::Observe)),
        "unsubscribe_sessions" => Some(ConnectionLocalRequirement::SelfOnly),
        "record_user_activity" => Some(ConnectionLocalRequirement::Grant(RpcMethodClass::Drive)),
        "detach_connection" => Some(ConnectionLocalRequirement::SelfOnly),
        "release_session" => Some(ConnectionLocalRequirement::Grant(
            RpcMethodClass::DeviceAdmin,
        )),
        "warm" => Some(ConnectionLocalRequirement::Grant(
            RpcMethodClass::DeviceAdmin,
        )),
        "host_response" => Some(ConnectionLocalRequirement::SelfOnly),
        "release_attachment" => Some(ConnectionLocalRequirement::SelfOnly),
        "reclaim_session" => Some(ConnectionLocalRequirement::Grant(
            RpcMethodClass::DeviceAdmin,
        )),
        "move_session" => Some(ConnectionLocalRequirement::Grant(
            RpcMethodClass::DeviceAdmin,
        )),
        "get_recovery_page" => Some(ConnectionLocalRequirement::SelfOnly),
        "release_recovery" => Some(ConnectionLocalRequirement::SelfOnly),
        "cancel_attempt" => Some(ConnectionLocalRequirement::SelfOnly),
        "reset_stream" => Some(ConnectionLocalRequirement::SelfOnly),
        _ => None,
    }
}

fn is_pre_declaration(record_type: &str) -> bool {
    matches!(
        record_type,
        "get_protocol_info" | "authenticate" | RPC_LAUNCH_PROOF_RECORD
    )
}

struct ConnectionState {
    proof_source: Option<LaunchProofSource>,
    proven: bool,
    spoken: bool,
    delegation: Option<DelegationRecord>,
    end_after_answer: bool,
}

pub type EndConnectionFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Per-connection delegation state of one host; keyed by the connection id the writer runs each
/// line under.
pub struct ConnectionDelegations {
    connections: Mutex<HashMap<String, ConnectionState>>,
    incarnation_max: i32,
    end_connection: Mutex<Option<EndConnectionFn>>,
}

impl Default for ConnectionDelegations {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl ConnectionDelegations {
    pub fn new(incarnation_max: Option<i32>, end_connection: Option<EndConnectionFn>) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            incarnation_max: incarnation_max.unwrap_or(INCARNATION_MAX),
            end_connection: Mutex::new(end_connection),
        }
    }

    pub fn set_end_connection(&self, end_connection: Option<EndConnectionFn>) {
        *self
            .end_connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = end_connection;
    }

    pub fn admit(&self, id: &str, proof_source: Option<LaunchProofSource>) {
        let mut guard = self
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // A re-registration under a live id keeps its state: forgetting a delegation would widen it.
        if guard.contains_key(id) {
            return;
        }
        guard.insert(
            id.to_owned(),
            ConnectionState {
                proof_source,
                proven: proof_source == Some(LaunchProofSource::Proven),
                spoken: false,
                delegation: None,
                end_after_answer: false,
            },
        );
    }

    /// Physical close (`omo-relay-daemon#307`): a delegated record's current word reads retired from
    /// now on, and the record stays, so a record still in flight never reads "not delegated".
    pub fn close(&self, id: &str) {
        let mut guard = self
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let has_delegation = guard
            .get(id)
            .is_some_and(|state| state.delegation.is_some());
        if !has_delegation {
            guard.remove(id);
        } else if let Some(state) = guard.get_mut(id)
            && let Some(ref mut delegation) = state.delegation
        {
            close_record(delegation);
        }
    }

    pub fn forget(&self, id: &str) {
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    pub fn record_of(&self, id: &str) -> Option<DelegationRecord> {
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .and_then(|s| s.delegation.clone())
    }

    /// What a line of connection `id` captures before its first await; `None` for a connection that
    /// never declared.
    pub fn descriptor_of(&self, id: &str) -> Option<DelegationDescriptor> {
        self.record_of(id).as_ref().map(descriptor_of)
    }

    pub fn view_of(&self, id: &str) -> Option<DelegatedConnectionView> {
        self.record_of(id).map(|record| DelegatedConnectionView {
            closed: record.closed,
            descriptor: descriptor_of(&record),
        })
    }

    /// After a line's answer is enqueued: ends the socket whose `detach_connection` exhausted its
    /// incarnations.
    pub fn line_answered(&self, id: Option<&str>) {
        let Some(id) = id else {
            return;
        };
        let should_end = {
            let mut guard = self
                .connections
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(state) = guard.get_mut(id)
                && state.end_after_answer
            {
                state.end_after_answer = false;
                true
            } else {
                false
            }
        };
        if should_end
            && let Some(cb) = self
                .end_connection
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .cloned()
        {
            cb(id);
        }
    }

    pub fn is_delegated(&self, id: Option<&str>) -> bool {
        let Some(id) = id else {
            return false;
        };
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .is_some_and(|s| s.delegation.is_some())
    }

    /// The answer to a line that is not JSON: today's parse error, typed `malformed` on a
    /// delegated connection.
    pub fn unparsable(&self, id: Option<&str>, error: &str) -> DelegationResponse {
        DelegationResponse {
            id: None,
            record_type: "response".to_owned(),
            command: "parse".to_owned(),
            success: false,
            data: None,
            error: Some(error.to_owned()),
            error_code: self
                .is_delegated(id)
                .then(|| DelegationRefusal::DelegationMalformed.as_str().to_owned()),
            error_data: None,
            extra: Map::new(),
        }
    }

    /// Decides one parsed inbound record of connection `id`. Synchronous: it runs before any await
    /// of the line.
    pub fn inspect(&self, id: Option<&str>, record: &Value) -> DelegationVerdict {
        let mut guard = self
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = id
            && !guard.contains_key(id)
        {
            guard.insert(
                id.to_owned(),
                ConnectionState {
                    proof_source: None,
                    proven: false,
                    spoken: false,
                    delegation: None,
                    end_after_answer: false,
                },
            );
        }
        let state = id.and_then(|conn_id| guard.get_mut(conn_id));
        let record_type = record
            .as_object()
            .and_then(|obj| obj.get("type"))
            .and_then(Value::as_str);

        if record_type == Some(RPC_LAUNCH_PROOF_RECORD)
            && state.as_ref().is_some_and(|s| s.proof_source.is_some())
        {
            if let Some(state) = state {
                state.proven = true;
            }
            return DelegationVerdict::Consumed;
        }
        if record_type == Some(DECLARE_DELEGATION_COMMAND) {
            return DelegationVerdict::Reply {
                response: Self::declaration(state, record),
            };
        }
        if record_type == Some(CLASSIFY_RECORD_COMMAND) {
            if let Some(state) = state {
                state.spoken = true;
            }
            return DelegationVerdict::Reply {
                response: classify_record(record),
            };
        }
        if let Some(cmd) = record_type.and_then(is_machine_op_command) {
            let proven_undelegated = state
                .as_ref()
                .is_some_and(|s| s.proven && s.delegation.is_none());
            if let Some(state) = state {
                state.spoken = true;
            }
            let req_id = request_id(record);
            return if proven_undelegated {
                DelegationVerdict::MachineOp {
                    id: req_id,
                    command: cmd,
                }
            } else {
                DelegationVerdict::Reply {
                    response: machine_op_refusal(req_id, cmd),
                }
            };
        }
        if record_type == Some(PRESENTER_CONTROL_INFO_COMMAND)
            && state.as_ref().is_none_or(|s| s.delegation.is_none())
        {
            let proven = state.as_ref().is_some_and(|s| s.proven);
            if let Some(state) = state {
                state.spoken = true;
            }
            let req_id = request_id(record);
            return if proven {
                DelegationVerdict::ControlInfo { id: req_id }
            } else {
                DelegationVerdict::Reply {
                    response: refused(
                        req_id,
                        PRESENTER_CONTROL_INFO_COMMAND,
                        "presenter_not_authorized",
                        "mcp_presenter_control_info needs a connection that proved the endpoint's launch key",
                        None,
                    ),
                }
            };
        }

        let Some(state) = state else {
            return DelegationVerdict::Pass;
        };
        if let Some(rt) = record_type
            && !is_pre_declaration(rt)
        {
            state.spoken = true;
        }
        let Some(ref mut delegation) = state.delegation else {
            return DelegationVerdict::Pass;
        };
        if let Some(rt) = record_type
            && is_private_presenter_type(rt)
        {
            return private_presenter_verdict(rt, request_id(record), &delegation.granted);
        }
        if let Some(refusal) = refuse_on_delegation(record, &delegation.granted) {
            return DelegationVerdict::Reply { response: refusal };
        }
        if record_type == Some("detach_connection")
            && !rotate_incarnation(delegation, Some(self.incarnation_max))
        {
            state.end_after_answer = true;
        }
        DelegationVerdict::Pass
    }

    fn declaration(state: Option<&mut ConnectionState>, record: &Value) -> DelegationResponse {
        let id = request_id(record);
        let parsed = match parse_declaration(record) {
            Ok(parsed) => parsed,
            Err(err) => {
                return refused(
                    id,
                    DECLARE_DELEGATION_COMMAND,
                    DelegationRefusal::DelegationMalformed.as_str(),
                    &err,
                    None,
                );
            }
        };
        let Some(state) = state.filter(|s| s.proven) else {
            return refused(
                id,
                DECLARE_DELEGATION_COMMAND,
                DelegationRefusal::DelegationUnauthenticated.as_str(),
                "declare_delegation needs a connection that proved the endpoint's launch key",
                None,
            );
        };
        if let Some(ref mut current) = state.delegation {
            if !narrow_record(current, &parsed) {
                return refused(
                    id,
                    DECLARE_DELEGATION_COMMAND,
                    DelegationRefusal::DelegationFixed.as_str(),
                    "a delegated connection keeps its principal and mode and can only narrow its grant and roles",
                    None,
                );
            }
        } else if state.spoken {
            return refused(
                id,
                DECLARE_DELEGATION_COMMAND,
                DelegationRefusal::DelegationLate.as_str(),
                "declare_delegation must come before any record other than get_protocol_info or authenticate",
                None,
            );
        } else {
            let record = create_record(
                parsed.principal.clone(),
                parsed.granted.clone(),
                parsed.roles.as_ref(),
            );
            state.delegation = Some(record);
        }

        let current = state
            .delegation
            .as_ref()
            .unwrap_or_else(|| unreachable!("delegation set above"));
        let mut granted: Vec<String> = parsed
            .granted
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect();
        granted.sort();
        let mut data = Map::new();
        data.insert(
            "granted".to_owned(),
            Value::Array(granted.into_iter().map(Value::String).collect()),
        );
        data.insert(
            "digest".to_owned(),
            Value::String(rpc_method_classes_digest_value().to_owned()),
        );
        if let Some(roles) = echoed_roles(current) {
            data.insert(
                "roles".to_owned(),
                Value::Array(roles.into_iter().map(Value::String).collect()),
            );
        }
        DelegationResponse::success(id, DECLARE_DELEGATION_COMMAND, Value::Object(data))
    }
}

fn refuse_on_delegation(
    record: &Value,
    granted: &HashSet<RpcMethodClass>,
) -> Option<DelegationResponse> {
    let obj = record.as_object();
    let id = obj
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(command) = obj.and_then(|o| o.get("type")).and_then(Value::as_str) else {
        return Some(refused(
            id,
            "parse",
            DelegationRefusal::DelegationMalformed.as_str(),
            "a record is a JSON object with a string type",
            None,
        ));
    };
    let Some(method_class) = classify_rpc_method(record) else {
        return Some(refused(
            id,
            command,
            DelegationRefusal::DelegationUnclassified.as_str(),
            &format!("the engine does not classify {command}"),
            None,
        ));
    };
    if method_class != RpcMethodClass::ConnectionLocal {
        return if granted.contains(&method_class) {
            None
        } else {
            Some(refused(
                id,
                command,
                DelegationRefusal::DelegationNotGranted.as_str(),
                &format!("{command} needs the {} grant", method_class.as_str()),
                Some(serde_json::json!({ "class": method_class.as_str() })),
            ))
        };
    }
    let Some(required) = connection_local_requirement(command) else {
        return Some(refused(
            id,
            command,
            DelegationRefusal::DelegationUnclassified.as_str(),
            &format!("the engine does not classify {command}"),
            None,
        ));
    };
    match required {
        ConnectionLocalRequirement::SelfOnly => None,
        ConnectionLocalRequirement::Grant(need) if granted.contains(&need) => None,
        ConnectionLocalRequirement::Grant(need) => Some(refused(
            id,
            command,
            DelegationRefusal::DelegationLocalOnly.as_str(),
            &format!(
                "{command} on a delegated connection needs the {} grant",
                need.as_str()
            ),
            Some(serde_json::json!({ "class": need.as_str() })),
        )),
    }
}

/// Ensures every `ConnectionLocal` entry in `RPC_METHOD_CLASS_ENTRIES` has a requirement mapping.
#[cfg(test)]
fn _all_connection_local_covered() -> bool {
    RPC_METHOD_CLASS_ENTRIES.iter().all(|(name, entry)| {
        !matches!(
            entry,
            RpcMethodClassEntry::Static(RpcMethodClass::ConnectionLocal)
        ) || connection_local_requirement(name).is_some()
    })
}
#[cfg(not(test))]
const _: () = {
    let _ = RPC_METHOD_CLASS_ENTRIES;
    let _ = std::mem::size_of::<RpcMethodClassEntry>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_connection_local_method_is_placed() {
        assert!(_all_connection_local_covered());
    }

    #[test]
    fn enforces_proof_late_narrowing_classes_and_detach_exhaustion() {
        let ended = Arc::new(Mutex::new(Vec::<String>::new()));
        let ended_cb = Arc::clone(&ended);
        let delegations = ConnectionDelegations::new(
            Some(2),
            Some(Arc::new(move |id| {
                ended_cb
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(id.to_owned());
            })),
        );

        delegations.admit("c1", Some(LaunchProofSource::InBand));
        // Cannot declare before proof.
        let unauth = delegations.inspect(
            Some("c1"),
            &json!({ "id": "1", "type": "declare_delegation", "principal": { "deviceId": "d" }, "granted": ["observe"] }),
        );
        assert!(matches!(
            unauth,
            DelegationVerdict::Reply { ref response } if response.error_code.as_deref() == Some("delegation_unauthenticated")
        ));

        // Pre-declaration records allowed before proof/declaration.
        assert_eq!(
            delegations.inspect(Some("c1"), &json!({ "type": "get_protocol_info" })),
            DelegationVerdict::Pass
        );
        assert_eq!(
            delegations.inspect(Some("c1"), &json!({ "type": RPC_LAUNCH_PROOF_RECORD })),
            DelegationVerdict::Consumed
        );

        // Declare host mode with observe + interactive_user.
        let ok = delegations.inspect(
            Some("c1"),
            &json!({
                "id": "2",
                "type": "declare_delegation",
                "principal": { "deviceId": "d" },
                "granted": ["observe"],
                "roles": ["interactive_user"]
            }),
        );
        assert!(matches!(
            ok,
            DelegationVerdict::Reply { ref response } if response.success
        ));
        assert!(delegations.is_delegated(Some("c1")));

        // Observe passes, drive is refused.
        assert_eq!(
            delegations.inspect(Some("c1"), &json!({ "type": "get_state" })),
            DelegationVerdict::Pass
        );
        let drive_refused =
            delegations.inspect(Some("c1"), &json!({ "type": "prompt", "message": "hi" }));
        assert!(matches!(
            drive_refused,
            DelegationVerdict::Reply { ref response } if response.error_code.as_deref() == Some("delegation_not_granted")
        ));

        // subscribe_sessions needs observe (passes), record_user_activity needs drive (refused).
        assert_eq!(
            delegations.inspect(Some("c1"), &json!({ "type": "subscribe_sessions" })),
            DelegationVerdict::Pass
        );
        let activity_refused = delegations.inspect(
            Some("c1"),
            &json!({ "type": "record_user_activity", "kind": "draft" }),
        );
        assert!(matches!(
            activity_refused,
            DelegationVerdict::Reply { ref response } if response.error_code.as_deref() == Some("delegation_local_only")
        ));

        // Detach once (1 -> 2), then twice (exhausted -> end_after_answer).
        assert_eq!(
            delegations.inspect(Some("c1"), &json!({ "type": "detach_connection" })),
            DelegationVerdict::Pass
        );
        delegations.line_answered(Some("c1"));
        assert!(
            ended
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );

        assert_eq!(
            delegations.inspect(Some("c1"), &json!({ "type": "detach_connection" })),
            DelegationVerdict::Pass
        );
        delegations.line_answered(Some("c1"));
        assert_eq!(
            *ended.lock().unwrap_or_else(PoisonError::into_inner),
            vec!["c1".to_owned()]
        );
    }

    #[test]
    fn machine_op_and_control_info_require_proven_non_delegated_connection() {
        let delegations = ConnectionDelegations::default();
        delegations.admit("host", Some(LaunchProofSource::Proven));
        assert!(matches!(
            delegations.inspect(Some("host"), &json!({ "id": "m1", "type": "machine_op" })),
            DelegationVerdict::MachineOp { .. }
        ));
        assert_eq!(
            delegations.inspect(
                Some("host"),
                &json!({ "id": "p1", "type": PRESENTER_CONTROL_INFO_COMMAND })
            ),
            DelegationVerdict::ControlInfo {
                id: Some("p1".to_owned())
            }
        );

        delegations.admit("unproven", None);
        assert!(matches!(
            delegations.inspect(Some("unproven"), &json!({ "id": "m2", "type": "machine_op" })),
            DelegationVerdict::Reply { ref response } if response.error_code.as_deref() == Some("machine_op_not_authorized")
        ));
    }
}
