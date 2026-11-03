//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-rpc-commands.ts
//! (v2026.10.27-2).
//!
//! The host commands a terminal control endpoint serves for its own session (senpi#3229):
//! `open_session` attaches by path or id, `subscribe_sessions`/`unsubscribe_sessions` run the host's
//! status stream over this one session, `steer`/`follow_up`/`prompt` go through the session's external
//! admission (the terminal's one delivery authority, so the user's draft still holds them) with the
//! host's `whenIdle` meaning, and `get_effect_status` reads the same effect ledger. The rest of the
//! host's drive set answers with the host's contract (senpi#4267): `set_client_info` declares the
//! connection's roles, `attach_session` and `release_attachment` take and drop a live attachment on the
//! session's identity, `abort` runs the host's effect path and its role rule (a Stop needs
//! `interactive_user`), and input sent with a `clientMessageId` is answered with an admission receipt
//! that a retry gets again, marked `replayed`. A frame this file does not own answers `NotOwned` and
//! falls through to the endpoint's other commands.
use crate::session_control_attach::{ControlAttachments, TerminalInputReceipt, key_of};
use crate::session_control_server::{ControlCommand, ControlConnection, failure, success};
use maho_core::agent_session::AgentSession;
use maho_rpc::connection_roles::CONNECTION_ROLE_FIXED;
use maho_rpc::custom_capability::INTERACTIVE_USER_CAPABILITY;
use maho_rpc::rpc_types::{
    RPC_ERROR_INVALID_SUBSCRIPTION, RPC_ERROR_SUBSCRIPTION_LIMIT, RPC_ERROR_UNKNOWN_SUBSCRIPTION,
};
use maho_rpc::session_status_stream::MAX_SUBSCRIPTIONS_PER_CONNECTION;
use maho_rpc::session_status_subscriptions::parse_subscription;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TUI_RPC_COMMANDS: [&str; 12] = [
    "open_session",
    "get_snapshot",
    "subscribe_sessions",
    "unsubscribe_sessions",
    "steer",
    "follow_up",
    "prompt",
    "get_effect_status",
    "set_client_info",
    "attach_session",
    "release_attachment",
    "abort",
];

/// The host's answer to a Stop from a connection that never declared the user's role (senpi#3811).
const STOP_REQUIRES_INTERACTIVE_CLIENT: &str =
    "stop_requires_interactive_client: declare interactive_user with set_client_info first";

pub struct RpcCommandContext {
    /// This endpoint's instance id: the `expectedHostInstance` an `attach_session` may name.
    pub instance_id: String,
    pub session: Arc<AgentSession>,
    pub attachments: Arc<ControlAttachments>,
    /// The composer's admission hold (senpi `externalAdmission.setEditorSource`): mhc's admission
    /// takes it per call instead of reading an installed source.
    pub editor_hold: Arc<dyn Fn() -> Value + Send + Sync>,
    /// The effect ledger's view of this session (`maho_rpc::agent_effect_session::AgentEffectSession`
    /// on a live endpoint). `None` serves only the plain abort and refuses the effect-bearing fields
    /// by name.
    pub effect_session: Option<Arc<dyn maho_rpc::effect_session::EffectSession>>,
}

/// senpi's `object | null | undefined`.
pub enum TuiRpcAnswer {
    /// The answer to write.
    Answer(Value),
    /// Answered on the record writer already.
    Answered,
    /// Not a command this file owns.
    NotOwned,
}

pub async fn run_tui_rpc_command(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> TuiRpcAnswer {
    use TuiRpcAnswer::{Answer, NotOwned};
    let kind = command["type"].as_str().unwrap_or_default();
    match kind {
        "get_snapshot" => Answer(get_snapshot(context, connection, command)),
        "open_session" => open_session(context, connection, command),
        "subscribe_sessions" => subscribe(context, connection, command),
        "unsubscribe_sessions" => Answer(unsubscribe(context, connection, command)),
        "steer" | "follow_up" => Answer(admit_input(
            context,
            command,
            if kind == "steer" { "steer" } else { "followUp" },
            &command["whenIdle"],
        )),
        "prompt" => Answer(admit_prompt(context, command)),
        "set_client_info" => Answer(set_client_info(context, connection, command)),
        "attach_session" => attach_session(context, connection, command),
        "release_attachment" => Answer(release_attachment(context, connection, command)),
        "abort" => Answer(abort(context, connection, command).await),
        "get_effect_status" => Answer(get_effect_status(context, command).await),
        _ => NotOwned,
    }
}

fn id_of(command: &ControlCommand) -> Option<&str> {
    command["id"].as_str()
}

fn answered(id: Option<&str>, command: &str, data: Value) -> Value {
    let mut answer = json!({"type": "response", "command": command, "success": true, "data": data});
    if let Some(id) = id {
        answer["id"] = id.into();
    }
    answer
}

/// Capture and mark before any response-writing await (senpi `getSnapshot`).
fn get_snapshot(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> Value {
    let RpcCommandContext {
        attachments,
        session,
        ..
    } = context;
    let refuse = |error: &str| json!({"id": command["id"], "type": "response", "command": "get_snapshot", "success": false, "error": error});
    if !command["sessionId"].is_null()
        && command["sessionId"].as_str() != Some(&attachments.handle())
    {
        return refuse("unknown_session");
    }
    let (tail, max_bytes) =
        match maho_rpc::get_snapshot::snapshot_bounds(command.get("tail"), command.get("maxBytes"))
        {
            Ok(bounds) => bounds,
            Err(error) => return refuse(&error),
        };
    let state = maho_rpc::connection_handler::build_rpc_session_state(session, None);
    let (data, marker_id) = session.with_session_manager(|manager| {
        maho_rpc::get_snapshot::build_snapshot(state, &manager.entries(), tail, max_bytes)
    });
    attachments.enqueue_for(
        connection,
        json!({"type": maho_rpc::get_snapshot::SNAPSHOT_MARKER, "markerId": marker_id}),
    );
    answered(id_of(command), "get_snapshot", data)
}

fn open_session(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> TuiRpcAnswer {
    let RpcCommandContext {
        session,
        attachments,
        ..
    } = context;
    let own_path = command["sessionPath"]
        .as_str()
        .is_some_and(|path| same_path(path, session.session_file().as_deref()));
    let own_id = !command["sessionId"].is_null()
        && command["sessionId"].as_str() == Some(session.session_id().as_str());
    if !own_path && !own_id {
        return TuiRpcAnswer::Answer(refusal(
            id_of(command),
            "open_session",
            "terminal_single_session",
            "this terminal hosts one session and the request names another",
            "open it by this terminal's sessionPath, or open the other session on a host",
        ));
    }
    attachments.attach(connection);
    attachments.enqueue_for(
        connection,
        json!({
            "id": command["id"],
            "type": "response",
            "command": "open_session",
            "success": true,
            "data": {
                "sessionId": attachments.handle(),
                "state": maho_rpc::connection_handler::build_rpc_session_state(session, None),
                "attached": true,
            },
        }),
    );
    TuiRpcAnswer::Answered
}

/// Declares this connection's roles, fixed once declared, exactly as on a host; the other fields
/// change nothing here.
fn set_client_info(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> Value {
    let id = command.get("id");
    let capabilities = &command["capabilities"];
    if capabilities.is_null() {
        return success(id, "set_client_info", None);
    }
    let Some(capabilities) = capabilities.as_array().and_then(|entries| {
        entries
            .iter()
            .map(|entry| entry.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
    }) else {
        return failure(
            id,
            "set_client_info",
            "capabilities must be an array of strings",
        );
    };
    let declared = context
        .attachments
        .roles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .declare(&key_of(connection), &capabilities);
    if !declared {
        return failure(id, "set_client_info", CONNECTION_ROLE_FIXED);
    }
    success(id, "set_client_info", None)
}

/// A live attachment on the session's identity, as on a host: `sessionId` is this terminal's routing
/// handle, `durableSessionId` its session id, and `expectedHostInstance`, when sent, this endpoint's
/// instance id. The answer follows the replay of the in-flight snapshot, then the session's records
/// reach the connection.
fn attach_session(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> TuiRpcAnswer {
    let RpcCommandContext {
        session,
        attachments,
        instance_id,
        ..
    } = context;
    let id = id_of(command);
    if command["sessionId"].as_str() != Some(attachments.handle().as_str()) {
        return TuiRpcAnswer::Answer(refusal(
            id,
            "attach_session",
            "unknown_session",
            "this terminal hosts one session and the request names another",
            "attach with this terminal's sessionId from list_sessions",
        ));
    }
    let instance_matches = command["expectedHostInstance"].is_null()
        || command["expectedHostInstance"].as_str() == Some(instance_id.as_str());
    if command["durableSessionId"].as_str() != Some(session.session_id().as_str())
        || !instance_matches
    {
        return TuiRpcAnswer::Answer(refusal(
            id,
            "attach_session",
            "session_identity_mismatch",
            "durableSessionId or expectedHostInstance does not name this terminal's session",
            "read list_sessions and get_protocol_info again, then attach with their ids",
        ));
    }
    attachments.attach(connection);
    attachments.enqueue_for(
        connection,
        json!({
            "id": command["id"],
            "type": "response",
            "command": "attach_session",
            "success": true,
            "data": {"sessionId": attachments.handle(), "attached": true},
        }),
    );
    TuiRpcAnswer::Answered
}

fn release_attachment(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> Value {
    if command["sessionId"].as_str() != Some(context.attachments.handle().as_str())
        || !context.attachments.detach(connection)
    {
        return refusal(
            id_of(command),
            "release_attachment",
            "session_not_attached",
            "this connection is not attached to that session",
            "attach first with attach_session or open_session",
        );
    }
    // Nothing outlives the terminal's own attachment, so a release is never deferred.
    success(
        command.get("id"),
        "release_attachment",
        Some(json!({"deferred": false})),
    )
}

/// The host's `abort`: the same effect receipts, and a Stop only from a connection that declared
/// `interactive_user`.
async fn abort(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> Value {
    let key = key_of(connection);
    let stop = command["intent"] == "stop";
    let interactive = context
        .attachments
        .roles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .has(Some(key.as_str()), INTERACTIVE_USER_CAPABILITY);
    if stop && !interactive {
        return failure(command.get("id"), "abort", STOP_REQUIRES_INTERACTIVE_CLIENT);
    }
    let session = context.session.clone();
    let dispatch: futures::future::BoxFuture<'static, ()> = Box::pin(async move {
        // The abort completes on its own; a host reports a failure out of band and never waits on it.
        if stop {
            session.stop().await;
        } else {
            session.abort().await;
        }
    });
    let Some(effects) = effect_context(context, key) else {
        // Without the effect ledger only the plain abort is served; what needs the ledger is refused
        // by name instead of dropped.
        if let Some(refused) =
            needs_effect_ledger(command, &["effectId", "awaitIdle", "clearQueue"])
        {
            return refused;
        }
        tokio::spawn(dispatch);
        return success(command.get("id"), "abort", None);
    };
    maho_rpc::effect_commands::run_abort(&effects, command, dispatch).await
}

/// `get_effect_status`: the host's ledger read, served once this session has an effect ledger.
async fn get_effect_status(context: &RpcCommandContext, command: &ControlCommand) -> Value {
    let Some(effects) = effect_context(context, String::new()) else {
        return needs_effect_ledger(command, &[])
            .unwrap_or_else(|| failure(command.get("id"), "get_effect_status", "unsupported"));
    };
    maho_rpc::effect_commands::run_get_effect_status(&effects, command).await
}

/// The host's effect-command context for this terminal's session, when it has an effect session.
fn effect_context(
    context: &RpcCommandContext,
    connection_id: String,
) -> Option<maho_rpc::effect_commands::EffectCommandContext> {
    Some(maho_rpc::effect_commands::EffectCommandContext {
        session: context.effect_session.clone()?,
        by: maho_rpc::effect_record::EffectActor { connection_id },
        session_id: Some(context.attachments.handle()),
    })
}

/// The refusal for a field (or, with no fields, a command) that needs the effect ledger.
fn needs_effect_ledger(command: &ControlCommand, fields: &[&str]) -> Option<Value> {
    let kind = command["type"].as_str().unwrap_or_default();
    let named = fields
        .iter()
        .filter(|field| !command[**field].is_null())
        .copied()
        .collect::<Vec<_>>();
    if !fields.is_empty() && named.is_empty() {
        return None;
    }
    let what = if named.is_empty() {
        kind.to_owned()
    } else {
        named.join(", ")
    };
    Some(refusal(
        id_of(command),
        kind,
        "unsupported_field",
        &format!("this terminal has no effect ledger yet, so it cannot serve {what}"),
        "send it without those fields, or send it to a host session",
    ))
}

/// senpi `refusal` (session-status-commands.ts): the stable `<code>: <reason>` error plus
/// `errorData { reason, action }`.
pub fn refusal(id: Option<&str>, command: &str, code: &str, reason: &str, action: &str) -> Value {
    let mut answer = json!({
        "type": "response",
        "command": command,
        "success": false,
        "error": format!("{code}: {reason}"),
        "errorCode": code,
        "errorData": {"reason": reason, "action": action},
    });
    if let Some(id) = id {
        answer["id"] = id.into();
    }
    answer
}

/// senpi `answerSubscribeSessions` over this terminal's one-session stream. A control connection
/// always has an identity, so the stdio refusal never applies here.
fn subscribe(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> TuiRpcAnswer {
    let key = key_of(connection);
    let attachments = &context.attachments;
    let id = id_of(command);
    let mut status = attachments
        .status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if status.connection_size(&key) >= MAX_SUBSCRIPTIONS_PER_CONNECTION {
        return TuiRpcAnswer::Answer(refusal(
            id,
            "subscribe_sessions",
            RPC_ERROR_SUBSCRIPTION_LIMIT,
            &format!(
                "this connection already holds {MAX_SUBSCRIPTIONS_PER_CONNECTION} subscriptions"
            ),
            "unsubscribe_sessions the ones you no longer read, then subscribe again",
        ));
    }
    let options = match parse_subscription(command) {
        Ok(options) => options,
        Err(refused) => {
            return TuiRpcAnswer::Answer(refusal(
                id,
                "subscribe_sessions",
                RPC_ERROR_INVALID_SUBSCRIPTION,
                &refused.reason,
                &refused.action,
            ));
        }
    };
    let subscribed = status.subscribe(
        &attachments.status_source,
        &attachments.status_delivery,
        &key,
        options,
    );
    let answer = answered(
        id,
        "subscribe_sessions",
        json!({
            "subscriptionId": subscribed.subscription_id,
            "snapshot": subscribed.snapshot,
            "seq": subscribed.seq,
            "hostGeneration": maho_rpc::protocol_identity::host_generation(&std::env::vars().collect()).to_string(),
            "hostInstanceId": maho_rpc::protocol_identity::host_instance_id(),
        }),
    );
    // As on a host: the answer reaches the connection's FIFO before the subscription goes live, so
    // its snapshot is the first record the client reads for this subscription. The stream stays
    // locked across both, so no delta lands between them.
    if attachments.respond(connection, &answer) && attachments.has_connection(connection) {
        status.activate(&attachments.status_delivery, &subscribed.subscription_id);
    } else {
        status.unsubscribe(&key, &subscribed.subscription_id);
    }
    TuiRpcAnswer::Answered
}

/// senpi `answerUnsubscribeSessions`.
fn unsubscribe(
    context: &RpcCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> Value {
    let removed = command["subscriptionId"]
        .as_str()
        .is_some_and(|subscription| {
            context
                .attachments
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unsubscribe(&key_of(connection), subscription)
        });
    if removed {
        return success(command.get("id"), "unsubscribe_sessions", None);
    }
    refusal(
        id_of(command),
        "unsubscribe_sessions",
        RPC_ERROR_UNKNOWN_SUBSCRIPTION,
        "this connection holds no subscription with that id",
        "send the subscriptionId your subscribe_sessions answer returned, on the same connection",
    )
}

fn admit_prompt(context: &RpcCommandContext, command: &ControlCommand) -> Value {
    let behavior = &command["streamingBehavior"];
    if behavior.is_null() && context.session.is_streaming() {
        return refusal(
            id_of(command),
            "prompt",
            "session_busy",
            "a turn is running and the prompt names no streamingBehavior",
            "send it with streamingBehavior \"steer\" or \"followUp\", or as a steer/follow_up",
        );
    }
    admit_input(
        context,
        command,
        if behavior == "followUp" {
            "followUp"
        } else {
            "steer"
        },
        &json!("start"),
    )
}

/// Input fields a host honours that admission cannot: refused by name instead of silently dropped.
const UNSERVED_INPUT_FIELDS: [&str; 6] = [
    "images",
    "thinkingLevel",
    "expandPromptTemplates",
    "unknownCommandAsText",
    "sessionTitlePrompt",
    "enqueueOrder",
];

fn same_path(requested: &str, own: Option<&str>) -> bool {
    let Some(own) = own else {
        return false;
    };
    let canonical = |path: &str| {
        let resolved = std::path::absolute(path).unwrap_or_else(|_| path.into());
        std::fs::canonicalize(&resolved).unwrap_or(resolved)
    };
    canonical(requested) == canonical(own)
}

fn admit_input(
    context: &RpcCommandContext,
    command: &ControlCommand,
    deliver_as: &str,
    when_idle: &Value,
) -> Value {
    let id = id_of(command);
    let kind = command["type"].as_str().unwrap_or_default();
    let unserved = UNSERVED_INPUT_FIELDS
        .iter()
        .filter(|field| !command[**field].is_null())
        .copied()
        .collect::<Vec<_>>();
    if !unserved.is_empty() {
        return refusal(
            id,
            kind,
            "unsupported_field",
            &format!(
                "a terminal delivers input through its admission, which carries text only, not {}",
                unserved.join(", ")
            ),
            "send the message without those fields, or send it to a host session",
        );
    }
    let Some(message) = command["message"]
        .as_str()
        .filter(|message| !message.trim().is_empty())
    else {
        return refusal(
            id,
            kind,
            "invalid_message",
            "message is empty",
            "send a non-empty message",
        );
    };
    if !when_idle.is_null() && when_idle != "queue" && when_idle != "start" {
        return refusal(
            id,
            kind,
            "invalid_when_idle",
            &format!("whenIdle must be \"queue\" or \"start\", not {when_idle}"),
            "send whenIdle: \"queue\" (the default) or \"start\"",
        );
    }
    let session = &context.session;
    let admission = session.external_admission();
    let client_message_id = command["clientMessageId"].as_str();
    let fingerprint = json!([kind, message, deliver_as, when_idle]).to_string();
    let prior = client_message_id.and_then(|client_message_id| {
        context
            .attachments
            .receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(client_message_id)
            .cloned()
    });
    if let Some(prior) = prior {
        if prior.fingerprint != fingerprint {
            return refusal(
                id,
                kind,
                "client_message_id_conflict",
                &format!(
                    "clientMessageId {} was already used for different input",
                    client_message_id.unwrap_or_default()
                ),
                "send new input with a new clientMessageId",
            );
        }
        return answered(
            id,
            kind,
            json!({
                "disposition": prior.disposition,
                "clientMessageId": client_message_id,
                "replayed": true,
                "duplicate": true,
                "admission": prior.admission,
            }),
        );
    }
    let expected = command["expected_turn_id"]
        .as_u64()
        .unwrap_or_else(|| admission.turn_epoch());
    let delivery_id = client_message_id.map_or_else(
        || format!("tui-rpc-{}", uuid::Uuid::new_v4()),
        str::to_owned,
    );
    let editor = (context.editor_hold)();
    let result = session.admit_external_message(
        &json!({
            "delivery_id": delivery_id,
            "text": message,
            "sender": {"kind": "external", "platform": "rpc"},
            "deliverAs": deliver_as,
            "expected_turn_id": expected,
            "whenIdle": if when_idle == "start" { "start" } else { "queue" },
        }),
        &editor,
    );
    let result = match result {
        Ok(result) => result,
        Err(error) => return failure(command.get("id"), kind, &error),
    };
    let admitted = |disposition: &'static str| {
        let Some(client_message_id) = client_message_id else {
            return answered(id, kind, json!({"disposition": disposition}));
        };
        let receipt = TerminalInputReceipt {
            fingerprint: fingerprint.clone(),
            disposition,
            admission: json!({
                "clientMessageId": client_message_id,
                "durableSessionId": session.session_id(),
                "disposition": disposition,
            }),
        };
        let admission = receipt.admission.clone();
        context
            .attachments
            .receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(client_message_id.to_owned(), receipt);
        answered(
            id,
            kind,
            json!({"disposition": disposition, "clientMessageId": client_message_id, "admission": admission}),
        )
    };
    match result["kind"].as_str().unwrap_or_default() {
        "started" => admitted("started"),
        "steered" | "queued" => admitted("queued"),
        "already_admitted" => {
            let mut data = json!({"disposition": "queued", "duplicate": true});
            if let Some(client_message_id) = client_message_id {
                data["clientMessageId"] = client_message_id.into();
            }
            answered(id, kind, data)
        }
        "held_draft" => refusal(
            id,
            kind,
            "held_draft",
            "the user is composing in this terminal, so nothing was delivered",
            "retry after the draft is sent or cleared",
        ),
        _ => refusal(
            id,
            kind,
            "turn_conflict",
            &format!(
                "the turn moved on (now {}); this steer named an earlier one",
                result["turn_epoch"]
            ),
            "read get_state's turn_epoch and send it again",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_owns_the_host_drive_set() {
        assert!(TUI_RPC_COMMANDS.contains(&"attach_session"));
        assert!(TUI_RPC_COMMANDS.contains(&"get_snapshot"));
        assert_eq!(
            answered(Some("1"), "prompt", json!({"disposition": "started"})),
            json!({"id":"1","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}})
        );
    }

    #[test]
    fn same_path_compares_canonical_spellings() {
        let directory = tempfile::tempdir().expect("directory");
        let file = directory.path().join("session.jsonl");
        std::fs::write(&file, "").expect("file");
        let dotted = directory.path().join(".").join("session.jsonl");
        assert!(same_path(
            &dotted.to_string_lossy(),
            Some(&file.to_string_lossy())
        ));
        assert!(!same_path(&file.to_string_lossy(), None));
    }
}
