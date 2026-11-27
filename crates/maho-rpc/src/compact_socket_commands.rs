//! senpi `modes/rpc/compact-socket-commands.ts`: validates full `set_client_info` declaration
//! before committing role, media, recovery or pending state, and enforces `compactAdmissionGate`.

use crate::compact_negotiation::{
    CompactSelection, DELEGATION_INCARNATION_STALE, RoleDeclarationVerdict,
};
use crate::connection_roles::CONNECTION_ROLE_FIXED;
use maho_core::engine_socket_flag::compact_updates_enabled;
use serde_json::{Value, json};

pub trait SocketCommandsHost: Send + Sync {
    fn current_connection(&self) -> Option<String>;
    fn current_incarnation(&self) -> Option<u64>;
    fn compact_selection(&self, peer: &str) -> Option<CompactSelection>;
    fn update_compact_selection(&self, peer: &str, f: &mut dyn FnMut(&mut CompactSelection));
    fn is_role_stale(&self, connection: &str, incarnation: Option<u64>) -> bool;
    fn declare_role(
        &self,
        connection: &str,
        caps: &[String],
        incarnation: Option<u64>,
    ) -> RoleDeclarationVerdict;
    fn accepts_remote_origin(&self, connection: &str, origin: &Value) -> bool;
    fn declare_remote_origin(&self, connection: &str, origin: &Value);
    fn registry_supports_compact(&self, open_options: Option<&Value>) -> bool;
    fn has_recovery_lane(&self, session_id: &str) -> bool;
    fn open_session_ids(&self) -> Vec<String>;
    fn set_pending_capabilities(&self, peer: &str, caps: Vec<String>);
    fn get_pending_capabilities(&self, peer: &str) -> Option<Vec<String>>;
    fn set_connection_capabilities(&self, peer: &str, caps: &[String]);
    fn set_sole_secret_capabilities(&self, caps: &[String]);
}

#[derive(Debug, Clone)]
pub struct SetClientInfoInput {
    pub id: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub remote_origin: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct GateCommandInput {
    pub id: Option<String>,
    pub command_type: String,
    pub session_id: Option<String>,
    pub admission_id: Option<String>,
    pub mirror: Option<String>,
    pub open_options: Option<Value>,
}

fn error_response(id: Option<&str>, command: &str, code: &str) -> Value {
    let mut resp = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": code,
        "errorCode": code,
    });
    if let Some(id) = id {
        resp["id"] = Value::String(id.to_owned());
    }
    resp
}

fn stale_error(id: Option<&str>, command: &str, message: &str) -> Value {
    let mut resp = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": message,
        "errorCode": DELEGATION_INCARNATION_STALE,
    });
    if let Some(id) = id {
        resp["id"] = Value::String(id.to_owned());
    }
    resp
}

/// Validate the full declaration before committing role, media, recovery or pending state.
pub fn declare_socket_client(host: &dyn SocketCommandsHost, command: &SetClientInfoInput) -> Value {
    declare_socket_client_with_flag(host, command, compact_updates_enabled())
}

pub fn declare_socket_client_with_flag(
    host: &dyn SocketCommandsHost,
    command: &SetClientInfoInput,
    enabled: bool,
) -> Value {
    let peer = host.current_connection();
    let connection = crate::connection_roles::role_connection(peer.as_deref()).to_owned();
    let incarnation = host.current_incarnation();
    if host.is_role_stale(&connection, incarnation) {
        return stale_error(
            command.id.as_deref(),
            "set_client_info",
            DELEGATION_INCARNATION_STALE,
        );
    }

    if let Some(origin) = &command.remote_origin
        && !host.accepts_remote_origin(&connection, origin)
    {
        return error_response(
            command.id.as_deref(),
            "set_client_info",
            "remote_origin_refused",
        );
    }

    let supported = host.registry_supports_compact(None);
    let mut ack: Option<Value> = None;
    let mut selected_caps: Option<Vec<String>> = None;
    let mut refusal: Option<&'static str> = None;

    if let Some(ref peer_id) = peer
        && host.compact_selection(peer_id).is_some()
    {
        let mut inner_refusal: Option<&'static str> = None;
        let mut inner_ack: Option<Value> = None;
        let mut inner_caps: Option<Vec<String>> = None;
        host.update_compact_selection(peer_id, &mut |sel| {
            let reason = sel.declare_with_flag(
                command.capabilities.as_deref(),
                supported,
                |caps| {
                    host.declare_role(&connection, caps, incarnation)
                        == RoleDeclarationVerdict::Declared
                },
                enabled,
            );
            inner_refusal = reason;
            if reason.is_none() {
                inner_ack = sel.ack();
                inner_caps = Some(sel.capabilities.clone());
            }
        });
        refusal = inner_refusal;
        ack = inner_ack;
        selected_caps = inner_caps;
    } else if let Some(ref caps) = command.capabilities
        && host.declare_role(&connection, caps, incarnation) != RoleDeclarationVerdict::Declared
    {
        refusal = Some("connection_role_fixed");
    }

    if let Some(reason) = refusal {
        let code = if reason == "connection_role_fixed" {
            CONNECTION_ROLE_FIXED
        } else {
            reason
        };
        return error_response(command.id.as_deref(), "set_client_info", code);
    }

    if let Some(origin) = &command.remote_origin {
        host.declare_remote_origin(&connection, origin);
    }
    if let Some(ref peer_id) = peer {
        let caps = if enabled {
            selected_caps
                .or_else(|| command.capabilities.clone())
                .or_else(|| host.get_pending_capabilities(peer_id))
                .unwrap_or_default()
        } else {
            command.capabilities.clone().unwrap_or_default()
        };
        host.set_pending_capabilities(peer_id, caps.clone());
        host.set_connection_capabilities(peer_id, &caps);
    } else if let Some(ref caps) = command.capabilities {
        host.set_sole_secret_capabilities(caps);
    }

    let mut resp = json!({
        "type": "response",
        "command": "set_client_info",
        "success": true,
    });
    if let Some(id) = &command.id {
        resp["id"] = Value::String(id.clone());
    }
    if let Some(ack_val) = ack {
        resp["data"] = ack_val;
    }
    resp
}

/// Format freezes before record-producing admission; status-only observation does not select a format.
pub fn compact_admission_gate(
    host: &dyn SocketCommandsHost,
    command: &GateCommandInput,
) -> Option<Value> {
    compact_admission_gate_with_flag(host, command, compact_updates_enabled())
}

pub fn compact_admission_gate_with_flag(
    host: &dyn SocketCommandsHost,
    command: &GateCommandInput,
    enabled: bool,
) -> Option<Value> {
    if !enabled {
        return None;
    }
    let content = matches!(
        command.command_type.as_str(),
        "open_session" | "attach_session" | "reclaim_session" | "reset_stream"
    ) || (command.command_type == "subscribe_sessions"
        && (command.admission_id.is_some()
            || command.mirror.as_deref().is_some_and(|m| m != "none")));
    if !content {
        return None;
    }

    let peer = host.current_connection();
    let selection = peer.as_deref().and_then(|p| host.compact_selection(p));
    let refuse = |reason: &str| -> Option<Value> {
        Some(error_response(
            command.id.as_deref(),
            &command.command_type,
            reason,
        ))
    };

    if selection.as_ref().is_some_and(|s| s.refused) {
        return refuse("compact_selection_refused");
    }
    if command.admission_id.is_some() && !selection.as_ref().is_some_and(|s| s.selected) {
        return refuse("compact_not_negotiated");
    }
    if selection.as_ref().is_some_and(|s| s.selected) {
        if matches!(
            command.command_type.as_str(),
            "open_session" | "attach_session" | "subscribe_sessions"
        ) && command.admission_id.is_none()
        {
            return refuse("compact_admission_required");
        }
        if command.command_type == "open_session"
            && !host.registry_supports_compact(command.open_options.as_ref())
        {
            return refuse("compact_endpoint_unsupported");
        }
        if command.command_type == "attach_session"
            && !command
                .session_id
                .as_deref()
                .is_some_and(|s| host.has_recovery_lane(s))
        {
            return refuse("compact_endpoint_unsupported");
        }
        if command.command_type == "subscribe_sessions"
            && host
                .open_session_ids()
                .iter()
                .any(|s| !host.has_recovery_lane(s))
        {
            return refuse("compact_endpoint_unsupported");
        }
    }
    if let Some(ref peer_id) = peer {
        host.update_compact_selection(peer_id, &mut |s| {
            s.admitted = true;
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_negotiation::COMPACT_UPDATES_CAPABILITY;
    use crate::socket_peer_recovery::EVENT_RESYNC_CAPABILITY;
    use std::sync::{Mutex, PoisonError};

    struct MockHost {
        stale: bool,
        sel: Mutex<CompactSelection>,
        caps: Mutex<Vec<String>>,
    }

    impl SocketCommandsHost for MockHost {
        fn current_connection(&self) -> Option<String> {
            Some("p1".into())
        }
        fn current_incarnation(&self) -> Option<u64> {
            Some(1)
        }
        fn compact_selection(&self, _: &str) -> Option<CompactSelection> {
            Some(
                self.sel
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            )
        }
        fn update_compact_selection(&self, _: &str, f: &mut dyn FnMut(&mut CompactSelection)) {
            let mut s = self.sel.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut s);
        }
        fn is_role_stale(&self, _: &str, _: Option<u64>) -> bool {
            self.stale
        }
        fn declare_role(&self, _: &str, _: &[String], _: Option<u64>) -> RoleDeclarationVerdict {
            RoleDeclarationVerdict::Declared
        }
        fn accepts_remote_origin(&self, _: &str, _: &Value) -> bool {
            true
        }
        fn declare_remote_origin(&self, _: &str, _: &Value) {}
        fn registry_supports_compact(&self, _: Option<&Value>) -> bool {
            true
        }
        fn has_recovery_lane(&self, _: &str) -> bool {
            true
        }
        fn open_session_ids(&self) -> Vec<String> {
            vec!["s1".into()]
        }
        fn set_pending_capabilities(&self, _: &str, caps: Vec<String>) {
            *self.caps.lock().unwrap_or_else(PoisonError::into_inner) = caps;
        }
        fn get_pending_capabilities(&self, _: &str) -> Option<Vec<String>> {
            None
        }
        fn set_connection_capabilities(&self, _: &str, _: &[String]) {}
        fn set_sole_secret_capabilities(&self, _: &[String]) {}
    }

    #[test]
    fn stale_declaration_refused_before_touching_selection() {
        let host = MockHost {
            stale: true,
            sel: Mutex::new(CompactSelection::new()),
            caps: Mutex::new(Vec::new()),
        };
        let resp = declare_socket_client_with_flag(
            &host,
            &SetClientInfoInput {
                id: Some("1".into()),
                capabilities: Some(vec![COMPACT_UPDATES_CAPABILITY.into()]),
                remote_origin: None,
            },
            true,
        );
        assert_eq!(resp["errorCode"], DELEGATION_INCARNATION_STALE);
        assert!(!host.compact_selection("p1").expect("sel").refused);
    }

    #[test]
    fn admission_gate_requires_admission_id_when_compact_selected() {
        let host = MockHost {
            stale: false,
            sel: Mutex::new(CompactSelection::new()),
            caps: Mutex::new(Vec::new()),
        };
        let ok = declare_socket_client_with_flag(
            &host,
            &SetClientInfoInput {
                id: Some("1".into()),
                capabilities: Some(vec![
                    COMPACT_UPDATES_CAPABILITY.into(),
                    EVENT_RESYNC_CAPABILITY.into(),
                ]),
                remote_origin: None,
            },
            true,
        );
        assert_eq!(ok["success"], true);

        let missing = compact_admission_gate_with_flag(
            &host,
            &GateCommandInput {
                id: Some("2".into()),
                command_type: "open_session".into(),
                session_id: None,
                admission_id: None,
                mirror: None,
                open_options: None,
            },
            true,
        )
        .expect("refusal");
        assert_eq!(missing["errorCode"], "compact_admission_required");

        let allowed = compact_admission_gate_with_flag(
            &host,
            &GateCommandInput {
                id: Some("3".into()),
                command_type: "open_session".into(),
                session_id: None,
                admission_id: Some("adm-1".into()),
                mirror: None,
                open_options: None,
            },
            true,
        );
        assert!(allowed.is_none());
        assert!(host.compact_selection("p1").expect("sel").admitted);
    }
}
