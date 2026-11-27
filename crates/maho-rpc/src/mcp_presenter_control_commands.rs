//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-control-commands.ts`.
//!
//! Signed management switch (`mcp_presenter_assign`, `mcp_presenter_acquire`, `mcp_presenter_revoke`).

use crate::mcp_form_types::McpPresenterKind;
use crate::mcp_presenter_authority::{
    McpPresenterAuthority, McpPresenterOwner, PRESENTER_NOT_AUTHORIZED,
};
use crate::mcp_presenter_grants::parse_features;
use crate::mcp_presenter_host_surface::PresenterHostSurface;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn run_presenter_command(
    command: &Value,
    host: &dyn PresenterHostSurface,
    authority: &McpPresenterAuthority,
    owner: &McpPresenterOwner,
    assigned_sessions: &mut BTreeSet<String>,
    registrations: &mut BTreeMap<String, String>,
) -> Result<Value, &'static str> {
    let id = command.get("id");
    let kind = command.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "mcp_presenter_assign" => {
            let session_id = command
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let binding_epoch = command
                .get("bindingEpoch")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let purpose = command
                .get("purpose")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let binding = host
                .get_mcp_form_binding(session_id)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            if binding.epoch != binding_epoch || purpose != "mcp_form" {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            assigned_sessions.insert(session_id.to_owned());
            Ok(json!({
                "type": "mcp_presenter_assigned",
                "id": id,
                "sessionId": session_id,
            }))
        }
        "mcp_presenter_acquire" => {
            let host_instance = command
                .get("hostInstance")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            if host_instance != authority.instance() {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            let session_id = command
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let binding_epoch = command
                .get("bindingEpoch")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let connection_nonce = command
                .get("connectionNonce")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let presenter_kind = match command.get("kind").and_then(Value::as_str) {
                Some("desktop") => McpPresenterKind::Desktop,
                Some("gateway") => McpPresenterKind::Gateway,
                _ => return Err(PRESENTER_NOT_AUTHORIZED),
            };
            let _features = parse_features(command.get("features").filter(|v| !v.is_null()))
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let binding = host
                .get_mcp_form_binding(session_id)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            let connection = host
                .find_mcp_presenter_connection(connection_nonce)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            if !assigned_sessions.contains(session_id) || binding.epoch != binding_epoch {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            let capabilities = host
                .get_connection_capabilities(&connection.id)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            if !capabilities.iter().any(|c| c == "interactive_user")
                || !capabilities.iter().any(|c| c == "mcp_form_presenter_v1")
            {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            let ctx = authority
                .connection_context(&connection, session_id)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            if command.get("purpose").and_then(Value::as_str) != Some("mcp_form")
                || command.get("attachmentId").and_then(Value::as_str)
                    != Some(ctx.attachment_id.as_str())
                || command.get("attachmentEpoch").and_then(Value::as_str)
                    != Some(ctx.attachment_epoch.as_str())
            {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            let reg_id = owner
                .register(&binding, &connection, presenter_kind)
                .map_err(|_| PRESENTER_NOT_AUTHORIZED)?;
            let grant = owner
                .acquire(&reg_id)
                .map_err(|_| PRESENTER_NOT_AUTHORIZED)?;
            registrations.insert(reg_id.clone(), session_id.to_owned());
            Ok(json!({
                "type": "mcp_presenter_acquired",
                "id": id,
                "registrationId": reg_id,
                "grantId": grant.grant_id,
                "scope": grant.scope,
            }))
        }
        "mcp_presenter_revoke" => {
            let reg_id = command
                .get("registrationId")
                .and_then(Value::as_str)
                .ok_or(PRESENTER_NOT_AUTHORIZED)?;
            if registrations.remove(reg_id).is_none() {
                return Err(PRESENTER_NOT_AUTHORIZED);
            }
            owner.revoke(reg_id);
            Ok(json!({
                "type": "mcp_presenter_revoked",
                "id": id,
                "registrationId": reg_id,
            }))
        }
        _ => Err(PRESENTER_NOT_AUTHORIZED),
    }
}
