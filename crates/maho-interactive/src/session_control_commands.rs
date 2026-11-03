//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-commands.ts (v2026.10.27-2).
//!
//! The terminal control endpoint's command surface. It answers who it is, which one session it holds
//! and that session's state and messages, renames it, streams its feed, wakes its inbox drain, relays
//! an answer to a question the session itself asked, and applies the pane's own session controls (a
//! model, a thinking level, an interrupt - `session_control_session_commands`), and serves the host's
//! observe-and-drive set over its one session (`session_control_rpc_commands`, senpi#3229,
//! senpi#4267). Input goes through the session's admission and only there. Every other command is
//! answered `unsupported` as data; `get_protocol_info` lists the accepted ones in `commands`.
use crate::session_control_attach::ControlAttachments;
use crate::session_control_feed::ControlFeed;
use crate::session_control_rpc_commands::{
    RpcCommandContext, TUI_RPC_COMMANDS, TuiRpcAnswer, run_tui_rpc_command,
};
use crate::session_control_server::{ControlCommand, ControlConnection, failure, success};
use crate::session_control_session_commands::{
    SESSION_CONTROL_COMMANDS, TuiSessionControls, run_session_control_command,
};
use futures::future::BoxFuture;
use maho_core::agent_session::AgentSession;
use maho_ext_api::{
    QuestionAnswer, QuestionCancelOrigin, QuestionResolvedBy, QuestionResponse, QuestionStatus,
};
use maho_rpc::custom_capability::{
    ENTRIES_TAIL_CAPABILITY, OBSERVE_READS_CAPABILITY, SESSION_STATUS_CAPABILITY,
    TUI_OBSERVER_CAPABILITY,
};
use maho_rpc::extension_ui_response::{
    answered_ui_request_id, settled_question_status, unanswered_question_ids,
};
use maho_rpc::ui_answer_ledger::{LedgerLookup, session_ui_answer_ledger};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub trait TuiControlSurface: TuiSessionControls {
    /// `"attachment"` / `"draft"` while the user composes; `None` otherwise.
    fn draft_hold(&self) -> Option<String>;
    fn blocking_question(&self) -> bool;
    fn pending_question_ids(&self) -> Vec<String>;
    /// The pending question's `questions` (wire shape), if this session is still waiting on it.
    fn pending_question(&self, request_id: &str) -> Option<Vec<Value>>;
    fn answer_question(&self, request_id: &str, response: QuestionResponse) -> bool;
    fn notice(&self, line: &str);
}

pub struct ControlCommandContext {
    pub instance_id: String,
    pub session: Arc<AgentSession>,
    pub surface: Arc<dyn TuiControlSurface>,
    pub feed: Arc<Mutex<ControlFeed>>,
    pub wake: Arc<dyn Fn(Vec<String>) -> BoxFuture<'static, Value> + Send + Sync>,
    pub attachments: Arc<ControlAttachments>,
    pub rpc: RpcCommandContext,
}

pub const TUI_CONTROL_CAPABILITY: &str = "tui_control";

const TUI_CONTROL_BASE_COMMANDS: [&str; 9] = [
    "get_protocol_info",
    "list_sessions",
    "get_state",
    "get_messages",
    "get_entries",
    "set_session_name",
    "subscribe",
    "wake",
    "extension_ui_response",
];

fn tui_control_commands() -> Vec<&'static str> {
    TUI_CONTROL_BASE_COMMANDS
        .iter()
        .chain(SESSION_CONTROL_COMMANDS.iter())
        .chain(TUI_RPC_COMMANDS.iter())
        .copied()
        .collect()
}

pub async fn run_control_command(
    context: &ControlCommandContext,
    connection: &ControlConnection,
    command: &ControlCommand,
) -> Option<Value> {
    let id = command.get("id");
    let kind = command["type"].as_str().unwrap_or_default();
    let session = &context.session;
    match kind {
        "get_protocol_info" => {
            let build = maho_core::engine_build_identity::engine_build_identity();
            Some(success(
                id,
                kind,
                Some(json!({
                    "protocolVersion": 1,
                    "serverVersion": build.text,
                    "capabilities": [
                        TUI_CONTROL_CAPABILITY,
                        SESSION_STATUS_CAPABILITY,
                        TUI_OBSERVER_CAPABILITY,
                        OBSERVE_READS_CAPABILITY,
                        ENTRIES_TAIL_CAPABILITY,
                        "snapshot_cursor",
                        "ui_answer_ledger",
                    ],
                    "mode": "tui",
                    "endpoint_kind": "tui",
                    "instanceId": context.instance_id,
                    "generation": 0,
                    "engineVersion": build.text,
                    "engineOrdinal": build.ordinal,
                    "commands": tui_control_commands(),
                })),
            ))
        }
        "list_sessions" => Some(success(
            id,
            kind,
            Some(json!({"sessions": [session_row(session)]})),
        )),
        "get_state" => {
            let mut state = maho_rpc::connection_handler::build_rpc_session_state(session, None);
            if let Some(state) = state.as_object_mut() {
                state.insert(
                    "turn_epoch".into(),
                    session.external_admission().turn_epoch().into(),
                );
                state.insert(
                    "blocking_question".into(),
                    context.surface.blocking_question().into(),
                );
                state.insert("compacting".into(), session.is_compacting().into());
                state.insert(
                    "editor_has_draft".into(),
                    context.surface.draft_hold().is_some().into(),
                );
                state.insert(
                    "state_version".into(),
                    lock(&context.feed).state_version().into(),
                );
            }
            Some(success(id, kind, Some(state)))
        }
        "get_messages" => Some(success(
            id,
            kind,
            Some(json!({"messages": session.messages()})),
        )),
        "get_entries" => Some(get_entries(session, command)),
        "set_session_name" => {
            let name = command["name"].as_str().map(str::trim).unwrap_or_default();
            if name.is_empty() {
                return Some(failure(id, kind, "Session name cannot be empty"));
            }
            session.set_session_name(name);
            Some(success(id, kind, None))
        }
        "subscribe" => {
            let cursor = command["cursor"].as_u64();
            let sender = connection.clone();
            let cursor = lock(&context.feed).subscribe(
                connection.id,
                cursor,
                Arc::new(move |event: &Value| sender.send(event.clone())),
            );
            Some(success(id, kind, Some(json!({"cursor": cursor}))))
        }
        "wake" => {
            let ids = command["delivery_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|entry| entry.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let result = (context.wake)(ids).await;
            let admitted = result.get("admitted").cloned().unwrap_or_else(|| json!([]));
            Some(success(id, kind, Some(json!({"admitted": admitted}))))
        }
        "extension_ui_response" => Some(answer_question(context, command)),
        _ => match run_tui_rpc_command(&context.rpc, connection, command).await {
            TuiRpcAnswer::Answer(answer) => Some(answer),
            TuiRpcAnswer::Answered => None,
            TuiRpcAnswer::NotOwned => Some(
                run_session_control_command(session, context.surface.as_ref(), command)
                    .await
                    .unwrap_or_else(|| failure(id, kind, "unsupported")),
            ),
        },
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn get_entries(session: &AgentSession, command: &ControlCommand) -> Value {
    let args = maho_rpc::get_entries_command::GetEntriesArgs {
        since: command["since"].as_str().map(str::to_owned),
        before: command["before"].as_str().map(str::to_owned),
        page: maho_rpc::get_entries_page::EntryPageRequest {
            limit: command.get("limit").cloned(),
            tail: command.get("tail").cloned(),
            max_bytes: command.get("maxBytes").cloned(),
        },
    };
    let answer = session.with_session_manager(|manager| {
        maho_rpc::get_entries_command::answer_get_entries(
            &manager.entries(),
            manager.leaf_id(),
            &args,
        )
    });
    match answer {
        Ok(data) => success(command.get("id"), "get_entries", Some(data)),
        Err(refused) => {
            let mut answer = failure(command.get("id"), "get_entries", &refused.message);
            answer["errorCode"] = refused.code.into();
            answer
        }
    }
}

fn session_row(session: &AgentSession) -> Value {
    let (cwd, created, last) = session.with_session_manager(|manager| {
        (
            manager.cwd().to_owned(),
            manager
                .header()
                .and_then(|header| header.get("timestamp").cloned())
                .unwrap_or(Value::Null),
            manager
                .entries()
                .last()
                .and_then(|entry| entry.get("timestamp").cloned()),
        )
    });
    json!({
        "sessionId": session.session_id(),
        "sessionPath": session.session_file(),
        "session_file": session.session_file(),
        "kind": "interactive",
        "surface": "tui",
        "cwd": cwd,
        "name": session.session_name(),
        "created_at": created,
        "updated_at": last.unwrap_or_else(|| created.clone()),
        "attachments": 1,
    })
}

/// Only a question this session asked, and is still waiting on, can be answered. `uiRequestId` names
/// it (the short form: `id`); the reply always carries the frame's `id`, as on a host. The answer
/// settles by the host's rule (`settled_question_status`), so the same frame reaches the model as the
/// same message on either surface.
fn answer_question(context: &ControlCommandContext, command: &ControlCommand) -> Value {
    let kind = "extension_ui_response";
    let frame_id = command.get("id");
    let answer_id = &command["answerId"];
    if !answer_id.is_null()
        && !answer_id
            .as_str()
            .is_some_and(|id| (1..=256).contains(&id.encode_utf16().count()))
    {
        return failure(frame_id, kind, "invalid_answer_id");
    }
    let answer_id = answer_id.as_str();
    let request_id = answered_ui_request_id(command);
    let ledger = session_ui_answer_ledger(&context.session.session_id());
    let receipt = match (request_id, answer_id) {
        (Some(request_id), Some(answer_id)) => lock(&ledger).lookup(request_id, Some(answer_id)),
        _ => None,
    };
    match receipt {
        Some(LedgerLookup::Replay) => {
            let mut answer = success(frame_id, kind, None);
            answer["disposition"] = "replay".into();
            return answer;
        }
        Some(LedgerLookup::AlreadyAnswered) => {
            let mut answer = failure(frame_id, kind, "already_answered");
            answer["disposition"] = "already_answered".into();
            return answer;
        }
        None => {}
    }
    let pending = request_id.and_then(|request_id| context.surface.pending_question(request_id));
    let (Some(request_id), Some(questions)) = (request_id, pending) else {
        let mut answer = failure(
            frame_id,
            kind,
            if answer_id.is_none() {
                "unknown_request"
            } else {
                "unknown_extension_ui_request"
            },
        );
        answer["disposition"] = "unknown".into();
        return answer;
    };
    let response = match question_response(command, &questions) {
        QuestionParse::Incomplete => return failure(frame_id, kind, "question_incomplete"),
        QuestionParse::Invalid => return failure(frame_id, kind, "invalid_response"),
        QuestionParse::Response(response) => response,
    };
    if !context.surface.answer_question(request_id, response) {
        return failure(frame_id, kind, "invalid_response");
    }
    lock(&ledger).record(request_id, answer_id);
    let mut answer = success(frame_id, kind, None);
    answer["disposition"] = "applied".into();
    answer
}

enum QuestionParse {
    Response(QuestionResponse),
    Incomplete,
    Invalid,
}

fn question_response(command: &ControlCommand, questions: &[Value]) -> QuestionParse {
    // Another client's cancel (the desktop's Remove) is the user's; the builtin settles it from its
    // own draft.
    if command["cancelled"] == true {
        return QuestionParse::Response(QuestionResponse {
            status: QuestionStatus::Cancelled,
            answers: BTreeMap::new(),
            comment: None,
            unanswered: Vec::new(),
            auto_resolved_after_ms: None,
            cancel_origin: Some(QuestionCancelOrigin::User),
            resolved_by: None,
        });
    }
    let Some(answers) = command["answers"].as_object() else {
        return QuestionParse::Invalid;
    };
    let mut parsed = BTreeMap::new();
    let mut wire = serde_json::Map::new();
    for (question, value) in answers {
        let Some(selected) = value.get("selected").and_then(Value::as_array) else {
            return QuestionParse::Invalid;
        };
        let selected = selected
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        let text = value.get("text").and_then(Value::as_str).map(str::to_owned);
        let mut entry = json!({"selected": selected});
        if let Some(text) = &text {
            entry["text"] = text.clone().into();
        }
        wire.insert(question.clone(), entry);
        parsed.insert(question.clone(), QuestionAnswer { selected, text });
    }
    let comment = command["comment"].as_str().map(str::to_owned);
    let wire = Value::Object(wire);
    let status = match settled_question_status(&wire, comment.as_deref()) {
        Some("comment-submitted") => QuestionStatus::CommentSubmitted,
        Some(_) => QuestionStatus::Answered,
        None => return QuestionParse::Incomplete,
    };
    QuestionParse::Response(QuestionResponse {
        status,
        answers: parsed,
        comment,
        unanswered: unanswered_question_ids(questions, &wire),
        auto_resolved_after_ms: None,
        cancel_origin: None,
        resolved_by: Some(QuestionResolvedBy::ControlEndpoint),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUESTION: &str =
        r#"{"id":"q1","header":"Ship","question":"Ship it?","options":[],"multiSelect":false}"#;

    fn questions() -> Vec<Value> {
        vec![serde_json::from_str(QUESTION).expect("question")]
    }

    #[test]
    fn protocol_info_lists_every_accepted_command() {
        let commands = tui_control_commands();
        assert_eq!(commands.first(), Some(&"get_protocol_info"));
        assert!(commands.contains(&"interrupt"));
        assert!(commands.contains(&"release_attachment"));
        assert_eq!(
            commands.len(),
            TUI_CONTROL_BASE_COMMANDS.len()
                + SESSION_CONTROL_COMMANDS.len()
                + TUI_RPC_COMMANDS.len()
        );
    }

    #[test]
    fn an_answer_settles_by_the_host_rule_and_names_its_resolver() {
        let QuestionParse::Response(response) = question_response(
            &json!({"answers": {"q1": {"selected": ["Yes"]}}}),
            &questions(),
        ) else {
            panic!("an answered question parses");
        };
        assert_eq!(response.status, QuestionStatus::Answered);
        assert_eq!(
            response.resolved_by,
            Some(QuestionResolvedBy::ControlEndpoint)
        );
        assert!(response.unanswered.is_empty());
        assert!(matches!(
            question_response(&json!({"answers": {}}), &questions()),
            QuestionParse::Incomplete
        ));
        assert!(matches!(
            question_response(&json!({"answers": {"q1": {"text": "x"}}}), &questions()),
            QuestionParse::Invalid
        ));
        let QuestionParse::Response(cancelled) =
            question_response(&json!({"cancelled": true}), &questions())
        else {
            panic!("a cancel parses");
        };
        assert_eq!(cancelled.cancel_origin, Some(QuestionCancelOrigin::User));
    }
}
