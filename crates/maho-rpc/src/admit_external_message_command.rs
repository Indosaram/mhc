//! Port of senpi `packages/coding-agent/src/modes/rpc/admit-external-message-command.ts`.
//!
//! `admit_external_message` (senpi#6233): admits a relayed external delivery into an
//! `AgentSession`, checking chain limits, sender authorization, remote origin stamping, and
//! transcript deduplication.

use crate::admit_external_message_wire::{
    ExternalAdmissionKind, RpcAdmitExternalMessageCommand, RpcAdmitExternalMessageReply,
};
use crate::chain_admission::{ChainAdmission, ChainRefusal, admit_input_chain};
use crate::remote_origin::RpcRemoteOrigin;
use crate::rpc_types::RPC_ERROR_SESSION_CLOSING;
use maho_core::agent_session::AgentSession;
use maho_core::external_admission::{DeliveryStamp, WRITTEN_DELIVERY_IDS};
use maho_core::turn_origin::{ConnectionOrigin, InputTurnMark};
use serde_json::{Value, json};

pub const SENDER_NOT_ALLOWED: &str = "sender_not_allowed";
pub const DELEGATED_ORIGIN_UNKNOWN: &str = "delegated_origin_unknown";

#[derive(Debug, Clone)]
pub struct DeliveryPeer {
    pub origin: Option<RpcRemoteOrigin>,
    pub connection_origin: ConnectionOrigin,
    pub turn_origin: Option<ConnectionOrigin>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdmitExternalMessageAnswer {
    Ok(RpcAdmitExternalMessageReply),
    Refused {
        message: String,
        error_code: String,
        error_data: Option<Value>,
    },
}

impl From<ChainRefusal> for AdmitExternalMessageAnswer {
    fn from(r: ChainRefusal) -> Self {
        Self::Refused {
            message: r.message,
            error_code: r.error_code.to_owned(),
            error_data: Some(r.error_data),
        }
    }
}

pub fn ledger_id(delivery_id: &str, origin: Option<&RpcRemoteOrigin>) -> String {
    match origin {
        None => format!("rpc:local/{delivery_id}"),
        Some(origin) => {
            let encoded: String =
                url::form_urlencoded::byte_serialize(origin.device_id.as_bytes()).collect();
            format!("device:{encoded}/{delivery_id}")
        }
    }
}

fn delivery_turn_mark(
    command: &RpcAdmitExternalMessageCommand,
    peer: &DeliveryPeer,
) -> InputTurnMark {
    match command.deliver_as {
        crate::admit_external_message_wire::ExternalDeliverAs::Steer => InputTurnMark::Join,
        crate::admit_external_message_wire::ExternalDeliverAs::FollowUp => {
            let origin = peer.turn_origin.unwrap_or(peer.connection_origin);
            match origin {
                ConnectionOrigin::Local => InputTurnMark::Local,
                ConnectionOrigin::Delegated => InputTurnMark::Delegated,
            }
        }
    }
}

pub fn admit_external_message(
    session: &AgentSession,
    command: &RpcAdmitExternalMessageCommand,
    peer: &DeliveryPeer,
    deliver: impl FnOnce(Value, &str) -> Result<(), String>,
) -> AdmitExternalMessageAnswer {
    let chain_val = command.chain_count.map(Value::from);
    match session.with_settings_manager(|s| admit_input_chain(chain_val.as_ref(), s)) {
        ChainAdmission::Refused(r) => return AdmitExternalMessageAnswer::from(r),
        ChainAdmission::Admitted { .. } => {}
    }
    if peer.connection_origin == ConnectionOrigin::Delegated && peer.origin.is_none() {
        return AdmitExternalMessageAnswer::Refused {
            message: format!(
                "{DELEGATED_ORIGIN_UNKNOWN}: a delegated connection that names no remote device cannot admit a delivery"
            ),
            error_code: DELEGATED_ORIGIN_UNKNOWN.to_owned(),
            error_data: None,
        };
    }
    if peer.origin.is_some()
        && command
            .sender
            .as_ref()
            .is_some_and(|s| s.kind == "command_line")
    {
        return AdmitExternalMessageAnswer::Refused {
            message: format!(
                "{SENDER_NOT_ALLOWED}: a remote device cannot send as this machine's command line"
            ),
            error_code: SENDER_NOT_ALLOWED.to_owned(),
            error_data: None,
        };
    }
    let admission = session.external_admission();
    if let Some(closed) = admission.closed_reason() {
        return AdmitExternalMessageAnswer::Refused {
            message: closed,
            error_code: RPC_ERROR_SESSION_CLOSING.to_owned(),
            error_data: None,
        };
    }
    let id = ledger_id(&command.delivery_id, peer.origin.as_ref());
    let written = session.with_session_manager(|sm| sm.reduce_history(&WRITTEN_DELIVERY_IDS));
    if written.contains(&id) {
        return AdmitExternalMessageAnswer::Ok(RpcAdmitExternalMessageReply {
            kind: ExternalAdmissionKind::AlreadyAdmitted,
            turn_id: admission.turn_epoch().to_string(),
            holds: None,
        });
    }
    let deliver_as = match command.deliver_as {
        crate::admit_external_message_wire::ExternalDeliverAs::Steer => "steer",
        crate::admit_external_message_wire::ExternalDeliverAs::FollowUp => "followUp",
    };
    let mut input = json!({
        "delivery_id": id,
        "text": command.text,
        "deliverAs": deliver_as,
    });
    if let Some(when_idle) = command.when_idle {
        input["whenIdle"] = json!(match when_idle {
            crate::admit_external_message_wire::ExternalWhenIdle::Start => "start",
            crate::admit_external_message_wire::ExternalWhenIdle::Queue => "queue",
        });
    }
    if let Some(expected) = &command.expected_turn_id
        && let Ok(epoch) = expected.parse::<u64>()
    {
        input["expected_turn_id"] = json!(epoch);
    }
    if let Some(display_text) = &command.display_text {
        input["displayText"] = json!(display_text);
    }
    if let Some(sender) = &command.sender {
        input["sender"] = serde_json::to_value(sender).unwrap_or(Value::Null);
    }
    let stamp = DeliveryStamp {
        remote_origin: peer
            .origin
            .as_ref()
            .and_then(|o| serde_json::to_value(o).ok()),
        chain_count: command.chain_count,
        turn_mark: Some(delivery_turn_mark(command, peer)),
    };
    let editor = json!({});
    let busy = session.is_streaming();
    match admission.admit_with_stamp_and_holds(&input, &editor, busy, &[], stamp, deliver) {
        Ok(res) => {
            let kind = match res["kind"].as_str().unwrap_or("queued") {
                "started" => ExternalAdmissionKind::Started,
                "held" => ExternalAdmissionKind::Held,
                "already_admitted" => ExternalAdmissionKind::AlreadyAdmitted,
                _ => ExternalAdmissionKind::Queued,
            };
            let turn_id = res["turn_epoch"].as_u64().unwrap_or(0).to_string();
            AdmitExternalMessageAnswer::Ok(RpcAdmitExternalMessageReply {
                kind,
                turn_id,
                holds: None,
            })
        }
        Err(err) => AdmitExternalMessageAnswer::Refused {
            message: err,
            error_code: RPC_ERROR_SESSION_CLOSING.to_owned(),
            error_data: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_id_namespaces_local_and_device() {
        assert_eq!(ledger_id("d1", None), "rpc:local/d1");
        let origin = RpcRemoteOrigin {
            device_id: "dev/1".to_owned(),
            device_name: None,
        };
        assert_eq!(ledger_id("d1", Some(&origin)), "device:dev%2F1/d1");
    }
}
