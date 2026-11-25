//! Extended P9 host command dispatch (`multi_session_host.rs`).
//!
//! Keeps `multi_session_host.rs` concise and under the per-file line limit while handling S01/S02
//! host-level and session-routed P9 commands (`machine_op*`, `find_session`, `park_session`,
//! `claim_session`, compact transport commands, `set_permission_preset`, `admit_external_message`,
//! `set_session_archived`, `set_session_settled`, and `get_project_*`/`set_project_*`/`read_*`/`write_*`).

use crate::compact_transport_refusal::{compact_transport_disabled, is_compact_transport_method};
use crate::find_session::{FindSessionPort, HostedSession, find_session};
use crate::machine_op::wire::MachineOpCommand;
use crate::multi_session_host::{CURRENT_CONNECTION, HostCore};
use crate::permission_preset_command::run_set_permission_preset;
use crate::rpc_types::{
    RPC_ERROR_MISSING_SESSION_ID, RPC_ERROR_UNKNOWN_SESSION, ResponseRecordType, RpcResponse,
    RpcResponseResult,
};
use crate::session_attribution::{SessionAttribution, run_with_session_attribution};
use crate::session_router_park::{claim_miss_response, park_error_response};
use serde_json::{Value, json};
use std::path::Path;

struct HostFindPort {
    agent_dir: std::path::PathBuf,
    hosted: Vec<HostedSession>,
}

impl FindSessionPort for HostFindPort {
    fn agent_dir(&self) -> Option<&Path> {
        Some(self.agent_dir.as_path())
    }

    fn hosted(&self) -> Vec<HostedSession> {
        self.hosted.clone()
    }
}

fn is_extended_session_command(kind: &str) -> bool {
    matches!(
        kind,
        "admit_external_message"
            | "set_permission_preset"
            | "set_session_archived"
            | "set_session_settled"
            | "get_project_trust"
            | "set_project_trust"
            | "get_project_settings"
            | "set_project_setting"
            | "read_project_file"
            | "write_project_file"
            | "get_project_mcp_servers"
            | "set_project_mcp_servers"
            | "read_project_skill_file"
            | "write_project_skill_file"
            | "read_user_skill_file"
            | "write_user_skill_file"
    )
}

async fn collect_hosted_sessions(core: &HostCore) -> Vec<HostedSession> {
    let rows = if let Some(workers) = &core.workers {
        workers.list_sessions(true)
    } else {
        core.registry.lock().await.list_sessions(true)
    };
    rows.into_iter()
        .map(|row| HostedSession {
            durable_session_id: row
                .get("durableSessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            session_path: row
                .get("sessionFile")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cwd: row.get("cwd").and_then(Value::as_str).map(str::to_owned),
        })
        .collect()
}

pub(crate) async fn handle_extended_host_command(
    core: &HostCore,
    connection: Option<&str>,
    value: &Value,
) -> std::io::Result<Option<()>> {
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return Ok(None);
    };
    let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    // 1. Machine ops (`senpi#6752`): host-level operations routed by op_id/sender.
    if let Some(cmd) = MachineOpCommand::from_str_name(kind) {
        let reply = core.machine_ops.answer(cmd, value, id.as_deref()).await;
        core.respond_record(
            connection,
            "",
            serde_json::to_value(reply).unwrap_or_default(),
        );
        return Ok(Some(()));
    }

    // 2. Host-level read/speculation commands.
    match kind {
        "find_session" => {
            let port = HostFindPort {
                agent_dir: core.options.agent_dir.clone(),
                hosted: collect_hosted_sessions(core).await,
            };
            let reply = find_session(
                &port,
                id.as_deref(),
                value.get("durableSessionId").and_then(Value::as_str),
            )
            .await;
            core.respond_record(connection, "", reply);
            return Ok(Some(()));
        }
        "park_session" => {
            let reply = park_error_response(value.get("id"), kind, "park_claim_disabled");
            core.respond_record(connection, "", reply);
            return Ok(Some(()));
        }
        "claim_session" => {
            let reply = claim_miss_response(value.get("id"));
            core.respond_record(connection, "", reply);
            return Ok(Some(()));
        }
        _ if is_compact_transport_method(kind) => {
            // Compact socket methods refuse with `feature_disabled` when compact updates are off.
            let reply = compact_transport_disabled(id.as_deref(), kind, session_id.as_deref());
            core.respond_record(connection, session_id.as_deref().unwrap_or_default(), reply);
            return Ok(Some(()));
        }
        _ => {}
    }

    if !is_extended_session_command(kind) {
        return Ok(None);
    }

    let Some(session_id) = session_id else {
        core.error(connection, id, kind, RPC_ERROR_MISSING_SESSION_ID, None);
        return Ok(Some(()));
    };

    let attached = core
        .attachments
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .owns(connection, &session_id);
    match crate::session_attachment_gate::attachment_gate(
        connection,
        kind,
        Some(&session_id),
        attached,
        || std::env::vars().collect(),
    ) {
        crate::session_attachment_gate::AttachmentGate::Pass => {}
        crate::session_attachment_gate::AttachmentGate::Warn(line) => eprintln!("{line}"),
        crate::session_attachment_gate::AttachmentGate::Refuse => {
            core.refuse(
                connection,
                id,
                kind,
                crate::rpc_types::RPC_ERROR_SESSION_NOT_ATTACHED,
                crate::session_attachment_gate::not_attached_detail(),
                Some(session_id),
            );
            return Ok(Some(()));
        }
    }

    if let Some(gate) = core.handover.get()
        && let Some(view) = gate.handover.view()
        && !gate.handover.admits(kind)
    {
        let refusal = crate::host_handover_wire::handover_refusal(id.as_deref(), kind, &view);
        core.respond_record(connection, "", refusal);
        return Ok(Some(()));
    }

    let _request = core.begin_request(Some(&session_id));

    if kind == "set_permission_preset" {
        let Some(preset) = value.get("permissionPreset").and_then(Value::as_str) else {
            core.error(
                connection,
                id,
                kind,
                "set_permission_preset requires 'permissionPreset'.",
                Some(session_id),
            );
            return Ok(Some(()));
        };
        if let Some(workers) = &core.workers {
            match workers.get_for_command(&session_id, kind, crate::connection_handler::now_ms()) {
                Err(error) => {
                    core.error(connection, id, kind, error.to_string(), Some(session_id));
                }
                Ok(worker) => {
                    if let Err(error) = run_with_session_attribution(
                        &core.activity,
                        SessionAttribution {
                            session_id: Some(session_id.clone()),
                            tool: None,
                            job: None,
                        },
                        worker.command(
                            value.clone(),
                            connection.map(str::to_owned),
                            core.declared_roles(connection),
                        ),
                    )
                    .await
                    {
                        core.error(connection, id, kind, error, Some(session_id));
                    }
                }
            }
            return Ok(Some(()));
        }

        let mut registry = core.registry.lock().await;
        let Some(entry) = registry.get_mut(&session_id) else {
            core.error(
                connection,
                id,
                kind,
                RPC_ERROR_UNKNOWN_SESSION,
                Some(session_id),
            );
            return Ok(Some(()));
        };
        entry.last_command_at = crate::connection_handler::now_ms();
        let Some(runtime_arc) = entry.runtime.as_mut() else {
            core.error(
                connection,
                id,
                kind,
                RPC_ERROR_UNKNOWN_SESSION,
                Some(session_id),
            );
            return Ok(Some(()));
        };
        let Some(runtime) = std::sync::Arc::get_mut(runtime_arc) else {
            core.error(
                connection,
                id,
                kind,
                "session_busy: retry set_permission_preset",
                Some(session_id),
            );
            return Ok(Some(()));
        };
        let mut resp = run_set_permission_preset(runtime, id, preset).await;
        resp.session_id = Some(session_id.clone());
        drop(registry);
        let value = serde_json::to_value(&resp).unwrap_or_else(|_| {
            serde_json::to_value(RpcResponse {
                id: None,
                record_type: ResponseRecordType::Response,
                command: kind.to_owned(),
                session_id: Some(session_id.clone()),
                result: RpcResponseResult::Success {
                    data: Some(json!({})),
                },
            })
            .unwrap_or_default()
        });
        core.respond_record(connection, &session_id, value);
        return Ok(Some(()));
    }

    if let Some(workers) = &core.workers {
        match workers.get_for_command(&session_id, kind, crate::connection_handler::now_ms()) {
            Err(error) => core.error(connection, id, kind, error.to_string(), Some(session_id)),
            Ok(worker) => {
                if let Err(error) = run_with_session_attribution(
                    &core.activity,
                    SessionAttribution {
                        session_id: Some(session_id.clone()),
                        tool: None,
                        job: None,
                    },
                    worker.command(
                        value.clone(),
                        connection.map(str::to_owned),
                        core.declared_roles(connection),
                    ),
                )
                .await
                {
                    core.error(connection, id, kind, error, Some(session_id));
                }
            }
        }
        return Ok(Some(()));
    }

    if let Some(entry) = core.registry.lock().await.get_mut(&session_id) {
        entry.last_command_at = crate::connection_handler::now_ms();
    }
    let binding = core.bindings.lock().await.get(&session_id).cloned();
    match binding {
        Some(binding) => {
            run_with_session_attribution(
                &core.activity,
                SessionAttribution {
                    session_id: Some(session_id.clone()),
                    tool: None,
                    job: None,
                },
                CURRENT_CONNECTION.scope(connection.map(str::to_owned), binding.handle(value)),
            )
            .await
            .map_err(std::io::Error::other)?;
            if kind == "set_session_archived" || kind == "set_session_settled" {
                core.registry.lock().await.sync_runtime_metadata();
            }
        }
        None => {
            core.error(
                connection,
                id,
                kind,
                RPC_ERROR_UNKNOWN_SESSION,
                Some(session_id),
            );
        }
    }
    Ok(Some(()))
}
