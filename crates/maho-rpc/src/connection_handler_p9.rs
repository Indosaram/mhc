//! Extended P9 command dispatch for single-session RPC bindings (`connection_handler.rs`).
//!
//! Keeps `connection_handler.rs` concise while providing full S01/S02 command coverage:
//! `admit_external_message`, `set_session_archived`, `set_session_settled`,
//! `get_project_*` / `set_project_*` / `read_project_*` / `write_project_*`,
//! `read_user_skill_file` / `write_user_skill_file`, `park_session` / `claim_session`,
//! and `project_expected_cwd` mismatch guard.

use crate::admit_external_message_command::{
    AdmitExternalMessageAnswer, DeliveryPeer, admit_external_message,
};
use crate::admit_external_message_wire::parse_admit_external_message_command;
use crate::project_expected_cwd::project_cwd_mismatch;
use crate::project_file_commands::{read_project_file, write_project_file};
use crate::project_mcp_commands::{get_project_mcp_servers, set_project_mcp_servers};
use crate::project_skill_commands::{read_project_skill_file, write_project_skill_file};
use crate::project_workspace_commands::{
    read_project_settings, read_project_trust, write_project_setting, write_project_trust,
};
use crate::project_write_gate::ProjectWriteRefusal;
use crate::rpc_types::{
    RPC_ERROR_INVALID_ARGUMENT, RPC_ERROR_MULTI_SESSION_DISABLED, ResponseRecordType, RpcResponse,
    RpcResponseResult,
};
use crate::user_skill_commands::{read_user_skill_file, write_user_skill_file};
use maho_core::agent_session::AgentSession;
use maho_core::session_archived::set_session_archived;
use maho_core::session_settled::{parse_session_settled, set_session_settled, settled_fields};
use maho_core::turn_origin::ConnectionOrigin;
use serde_json::{Value, json};

fn ok_response(
    id: Option<String>,
    command: &str,
    session_id: Option<String>,
    data: Option<Value>,
) -> RpcResponse {
    RpcResponse {
        id,
        record_type: ResponseRecordType::Response,
        command: command.to_owned(),
        session_id,
        result: RpcResponseResult::Success { data },
    }
}

fn err_response(
    id: Option<String>,
    command: &str,
    session_id: Option<String>,
    error: String,
    error_code: Option<String>,
    error_data: Option<Value>,
) -> RpcResponse {
    RpcResponse {
        id,
        record_type: ResponseRecordType::Response,
        command: command.to_owned(),
        session_id,
        result: RpcResponseResult::Error {
            error,
            error_code,
            error_data,
        },
    }
}

fn project_refusal_response(
    id: Option<String>,
    command: &str,
    session_id: Option<String>,
    err: ProjectWriteRefusal,
) -> RpcResponse {
    err_response(
        id,
        command,
        session_id,
        err.message,
        Some(err.error_code),
        err.error_data,
    )
}

/// Checks `expectedCwd` mismatch before any session command runs.
pub fn check_expected_cwd(session: &AgentSession, value: &Value) -> Option<RpcResponse> {
    let command = value["type"].as_str().unwrap_or_default();
    let expected = value.get("expectedCwd");
    let session_cwd = session.cwd();
    let mismatch = project_cwd_mismatch(command, expected, &session_cwd)?;
    Some(err_response(
        value.get("id").and_then(Value::as_str).map(str::to_owned),
        command,
        value
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        mismatch.message.to_owned(),
        Some(mismatch.error_code.to_owned()),
        None,
    ))
}

/// Dispatches P9 commands that extend `RpcCommandBody`, returning `None` for standard `RpcCommandBody` variants.
pub async fn handle_extended_session_command(
    session: &AgentSession,
    value: &Value,
    sink: Option<&crate::connection_handler::RpcRecordSink>,
) -> Option<RpcResponse> {
    let kind = value.get("type").and_then(Value::as_str)?;
    let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned);

    if let Some(refusal) = check_expected_cwd(session, value) {
        return Some(refusal);
    }

    match kind {
        "reset_stream" | "cancel_attempt" | "get_recovery_page" | "release_recovery" => {
            let reply = crate::compact_transport_refusal::compact_transport_disabled(
                id.as_deref(),
                kind,
                session_id.as_deref(),
            );
            Some(RpcResponse {
                id,
                record_type: ResponseRecordType::Response,
                command: kind.to_owned(),
                session_id,
                result: RpcResponseResult::Error {
                    error: reply["error"]
                        .as_str()
                        .unwrap_or("feature_disabled")
                        .to_owned(),
                    error_code: reply["errorCode"].as_str().map(str::to_owned),
                    error_data: reply.get("errorData").cloned(),
                },
            })
        }
        "park_session" | "claim_session" => Some(err_response(
            id,
            kind,
            session_id,
            RPC_ERROR_MULTI_SESSION_DISABLED.to_owned(),
            None,
            None,
        )),
        "set_session_archived" => {
            let Some(archived) = value.get("archived").and_then(Value::as_bool) else {
                return Some(err_response(
                    id,
                    kind,
                    session_id,
                    "set_session_archived requires a boolean 'archived'.".to_owned(),
                    Some(RPC_ERROR_INVALID_ARGUMENT.to_owned()),
                    None,
                ));
            };
            session.with_session_manager_mut(|manager| set_session_archived(manager, archived));
            if let Some(sink) = sink {
                sink(json!({ "type": "session_archived_changed", "archived": archived }));
            }
            Some(ok_response(id, kind, session_id, None))
        }
        "set_session_settled" => {
            match parse_session_settled(value.get("settled"), value.get("by")) {
                Err(error) => Some(err_response(
                    id,
                    kind,
                    session_id,
                    error,
                    Some(RPC_ERROR_INVALID_ARGUMENT.to_owned()),
                    None,
                )),
                Ok(parsed) => {
                    session
                        .with_session_manager_mut(|manager| set_session_settled(manager, &parsed));
                    if let Some(sink) = sink {
                        let fields = settled_fields(&parsed);
                        let mut ev = json!({
                            "type": "session_settled_changed",
                            "settled": fields.settled,
                        });
                        if let Some(by) = fields.settled_by
                            && let Ok(val) = serde_json::to_value(by)
                        {
                            ev["settledBy"] = val;
                        }
                        sink(ev);
                    }
                    Some(ok_response(id, kind, session_id, None))
                }
            }
        }
        "admit_external_message" => {
            let cmd = match parse_admit_external_message_command(value) {
                Ok(cmd) => cmd,
                Err(message) => {
                    return Some(err_response(
                        id,
                        kind,
                        session_id,
                        message,
                        Some(RPC_ERROR_INVALID_ARGUMENT.to_owned()),
                        None,
                    ));
                }
            };
            let peer = DeliveryPeer {
                origin: None,
                connection_origin: ConnectionOrigin::Local,
                turn_origin: Some(ConnectionOrigin::Local),
            };
            let deliver = |_input: Value, _id: &str| Ok(());
            match admit_external_message(session, &cmd, &peer, deliver) {
                AdmitExternalMessageAnswer::Ok(reply) => {
                    let data = serde_json::to_value(reply).ok();
                    Some(ok_response(id, kind, session_id, data))
                }
                AdmitExternalMessageAnswer::Refused {
                    message,
                    error_code,
                    error_data,
                } => Some(err_response(
                    id,
                    kind,
                    session_id,
                    message,
                    Some(error_code),
                    error_data,
                )),
            }
        }
        "get_project_trust" => {
            let data = serde_json::to_value(read_project_trust(session).await).ok();
            Some(ok_response(id, kind, session_id, data))
        }
        "set_project_trust" => {
            let res = write_project_trust(session, value.get("choice"), |fut| fut, None).await;
            Some(match res {
                Ok(data) => ok_response(id, kind, session_id, serde_json::to_value(data).ok()),
                Err(err) => project_refusal_response(id, kind, session_id, err),
            })
        }
        "get_project_settings" => {
            let data = serde_json::to_value(read_project_settings(session)).ok();
            Some(ok_response(id, kind, session_id, data))
        }
        "set_project_setting" => {
            match write_project_setting(session, value.get("key"), value.get("value")).await {
                Ok(data) => Some(ok_response(
                    id,
                    kind,
                    session_id,
                    serde_json::to_value(data).ok(),
                )),
                Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
            }
        }
        "read_project_file" => match read_project_file(session, value.get("path")).await {
            Ok(data) => Some(ok_response(
                id,
                kind,
                session_id,
                serde_json::to_value(data).ok(),
            )),
            Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
        },
        "write_project_file" => {
            match write_project_file(
                session,
                value.get("path"),
                value.get("content"),
                value.get("ifEtag"),
                None,
            )
            .await
            {
                Ok(data) => Some(ok_response(
                    id,
                    kind,
                    session_id,
                    serde_json::to_value(data).ok(),
                )),
                Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
            }
        }
        "get_project_mcp_servers" => match get_project_mcp_servers(session).await {
            Ok(data) => Some(ok_response(
                id,
                kind,
                session_id,
                serde_json::to_value(data).ok(),
            )),
            Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
        },
        "set_project_mcp_servers" => {
            match set_project_mcp_servers(
                session,
                value.get("patch"),
                value.get("ifEtag"),
                None,
                None,
            )
            .await
            {
                Ok(data) => Some(ok_response(
                    id,
                    kind,
                    session_id,
                    serde_json::to_value(data).ok(),
                )),
                Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
            }
        }
        "read_project_skill_file" => {
            match read_project_skill_file(session, value.get("skill")).await {
                Ok(data) => Some(ok_response(
                    id,
                    kind,
                    session_id,
                    serde_json::to_value(data).ok(),
                )),
                Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
            }
        }
        "write_project_skill_file" => {
            match write_project_skill_file(
                session,
                value.get("skill"),
                value.get("content"),
                value.get("ifEtag"),
                None,
            )
            .await
            {
                Ok(data) => Some(ok_response(
                    id,
                    kind,
                    session_id,
                    serde_json::to_value(data).ok(),
                )),
                Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
            }
        }
        "read_user_skill_file" => match read_user_skill_file(session, value.get("skill")).await {
            Ok(data) => Some(ok_response(
                id,
                kind,
                session_id,
                serde_json::to_value(data).ok(),
            )),
            Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
        },
        "write_user_skill_file" => {
            match write_user_skill_file(
                session,
                value.get("skill"),
                value.get("content"),
                value.get("ifEtag"),
                None,
                None,
            )
            .await
            {
                Ok(data) => Some(ok_response(
                    id,
                    kind,
                    session_id,
                    serde_json::to_value(data).ok(),
                )),
                Err(err) => Some(project_refusal_response(id, kind, session_id, err)),
            }
        }
        _ => None,
    }
}
