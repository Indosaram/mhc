use crate::{family::tool_name, format::{format_result_details, format_result_text, format_user_message}, notify::*, pending::PendingTimer, registry::*, schema::{AskUserVariant, claude_params, codex_params, to_canonical}};
use maho_ext_api::*;
use maho_ai::types::TextContent;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct AskUserState { pub timed_out: bool, pub unavailable: bool }
fn unavailable(request: &QuestionRequest) -> QuestionResponse {
    QuestionResponse { status: QuestionStatus::Unavailable, answers: Default::default(), comment: None, unanswered: request.questions.iter().map(|question| question.id.clone()).collect(), auto_resolved_after_ms: None }
}
fn result(variant: AskUserVariant, request: &QuestionRequest, response: &QuestionResponse) -> AgentToolResult {
    AgentToolResult { content: vec![ContentBlock::Text(TextContent { text: format_result_text(response, &request.questions), audience: None, text_signature: None })], details: format_result_details(variant, response, &request.questions), usage: None, added_tool_names: None, terminate: None, is_error: None }
}
fn publish(owner: QuestionOwner, request: &QuestionRequest, response: &QuestionResponse, variant: AskUserVariant) {
    if response.status == QuestionStatus::TimedOut { owner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).timed_out = true; }
    owner.sender.events.emit("herdr:blocked", &json!({"active":false,"id":request.request_id}));
    if !request.wait_for_answer {
        if let Err(error) = owner.sender.append_entry(ASK_USER_SETTLEMENT_ENTRY, Some(json!({"requestId":request.request_id,"status":crate::format::status_name(response.status)}))) { owner.context.ui.notify(&error.message, NotificationType::Error); }
        if response.status != QuestionStatus::Cancelled
            && let Err(error) = owner.sender.send_user_message(UserMessageContent::Text(format_user_message(response, &request.request_id, &request.questions)), SendUserMessageOptions { deliver_as: Some(if owner.context.is_idle() { StreamingBehavior::FollowUp } else { StreamingBehavior::Steer }), expand_prompt_templates: false }) {
            owner.context.ui.notify(&error.message, NotificationType::Error);
        }
    }
    emit_notification(&owner.sender.events, request, response, variant);
}
pub fn register_tool(api: &mut ExtensionApi, variant: AskUserVariant, state: Arc<Mutex<AskUserState>>, sender: Arc<ExtensionApi>) {
    let params = match variant { AskUserVariant::Codex => codex_params(), AskUserVariant::Claude => claude_params() };
    let mut definition = ToolDefinition::new(tool_name(variant), "Ask a material question; choose explicitly whether to wait or receive the answer later.", params, Arc::new(|_| Box::pin(async { Err(ToolError::Message("Extension context required".into())) })));
    definition.label = "Ask user".into();
    definition.allow_lazy_activation = Some(false);
    definition.prepare_arguments = Some(Arc::new(move |args| { to_canonical(variant, &args, String::new(), None).map_err(ToolError::Message)?; Ok(args) }));
    if let Err(error) = api.register_tool_with_extension_context(definition, Arc::new(move |id, args, signal, _, ctx| {
        let state = state.clone(); let sender = sender.clone();
        Box::pin(async move {
            let settings = ctx.get_ask_user_settings()?;
            let timeout = (settings.timeout_minutes * 60_000.0) as u64;
            let request = to_canonical(variant, &args, id.into(), Some(timeout)).map_err(ExtensionFailure::new)?;
            let unavailable_now = {
                let current = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                current.timed_out || current.unavailable || !settings.enabled || sender.get_flag("no-ask-user") == Some(FlagValue::Boolean(true)) || matches!(ctx.mode, ExtensionMode::Print | ExtensionMode::Json)
            };
            if unavailable_now { return Ok(result(variant, &request, &unavailable(&request))); }
            let session = ctx.session_manager.session_id().to_owned();
            if let Some(existing) = get_pending_questions(&session).into_iter().find(|entry| entry.request.request_id == id) {
                let mut completion = existing.completion.clone();
                loop {
                    if let Some(response) = completion.borrow().clone() { return Ok(result(variant, &request, &response)); }
                    completion.changed().await.map_err(|_| ExtensionFailure::new("Question owner disappeared"))?;
                }
            }
            sender.append_entry(ASK_USER_QUESTION_ENTRY, Some(json!({"requestId":id,"headers":request.questions.iter().map(|question|&question.header).collect::<Vec<_>>()})))?;
            let (settled, mut completion) = tokio::sync::watch::channel(None::<QuestionResponse>);
            let (terminal, terminal_response) = tokio::sync::watch::channel(None::<QuestionResponse>);
            let (owner, mut owners) = tokio::sync::watch::channel(Some(QuestionOwner { sender: sender.clone(), context: ctx.clone(), state: state.clone() }));
            let (publishing, _) = tokio::sync::watch::channel(false);
            let publication = Arc::new(Mutex::new(publishing));
            let cancel_signal = AbortSignal::default();
            let callback = terminal.clone();
            let timer = Arc::new(PendingTimer::new(request.clone(), Arc::new(move |response| { callback.send_if_modified(|value| { if value.is_some() { false } else { *value = Some(response); true } }); })));
            let cancelled = terminal.clone(); let pending = timer.clone(); let cancel = cancel_signal.clone();
            register_pending_question(&session, Arc::new(PendingQuestionEntry { request: request.clone(), completion: completion.clone(), owner, publication: publication.clone(), cancel: Arc::new(move |reason| { let response = pending.cancel(reason); cancelled.send_if_modified(|value| { if value.is_some() { false } else { *value = Some(response); true } }); cancel.abort(); }) }));
            sender.events.emit(ASK_USER_ASKED_EVENT, &json!({"requestId":id,"variant":match variant {AskUserVariant::Codex=>"codex",AskUserVariant::Claude=>"claude"}}));
            sender.events.emit("herdr:blocked", &json!({"active":true,"id":id,"label":request.questions.first().map(|q|format!("{} — {}",q.header,q.question)).unwrap_or_default()}));
            let owner_request = request.clone();
            let work_completion = terminal_response;
            let task = tokio::spawn(async move {
                let mut completed = work_completion;
                let response = loop {
                    let current = owners.borrow_and_update().clone();
                    let progress = timer.clone();
                    let remaining_ms = timer.remaining_ms();
                    let attachment_signal = AbortSignal::default();
                    let ui_signal = attachment_signal.clone();
                    let request = owner_request.clone();
                    let question = async move {
                        match current {
                            Some(owner) => owner.context.ui.question(request.clone(), QuestionOptions { dialog: ExtensionUiDialogOptions { signal: Some(ui_signal), timeout_ms: Some(remaining_ms) }, on_progress: Some(Arc::new(move |draft| progress.touch(Some((draft.answers.unwrap_or_default(), draft.comment))))) }).await,
                            None => std::future::pending().await,
                        }
                    };
                    let response = tokio::select! {
                        biased;
                        update = owners.changed() => { attachment_signal.abort(); if update.is_err() { Some(timer.cancel(QuestionStatus::Cancelled)) } else { None } },
                        () = async { if let Some(signal) = &signal { signal.cancelled().await; } else { std::future::pending::<()>().await; } } => Some(timer.cancel(QuestionStatus::Cancelled)),
                        () = async { loop { if completed.borrow().is_some() || completed.changed().await.is_err() { break; } } } => Some(completed.borrow().clone().unwrap_or_else(|| timer.cancel(QuestionStatus::Cancelled))),
                        response = question => Some(match response { Ok(response) => response, Err(error) => { if let Some(owner) = owners.borrow().as_ref() { owner.context.ui.notify(&format!("Question UI failed: {error}"), NotificationType::Error); } timer.cancel(QuestionStatus::Cancelled) } }),
                    };
                    attachment_signal.abort();
                    if let Some(response) = response { break response; }
                };
                match response.status {
                    QuestionStatus::Answered | QuestionStatus::CommentSubmitted => { timer.submit(response.answers.clone(), response.comment.clone()); },
                    other => { timer.cancel(other); },
                }
                cancel_signal.abort();
                let owner = select_publication(&owners, &publication, || {
                    let request = owner_request.clone(); let outcome = response.clone();
                    queue_outcome(&session, Box::new(move |owner| publish(owner, &request, &outcome, variant)));
                });
                if let Some(owner) = owner { publish(owner, &owner_request, &response, variant); }
                publication.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send_replace(false);
                unregister_pending_question(&session, &owner_request.request_id);
                settled.send_replace(Some(response));
            });
            if !request.wait_for_answer { return Ok(AgentToolResult { content: vec![ContentBlock::Text(TextContent { text:"Question accepted; the answer will arrive as a user message.".into(), audience:None, text_signature:None })], details:json!({"accepted":true,"requestId":id,"status":"pending"}), usage:None,added_tool_names:None,terminate:None,is_error:None }); }
            loop {
                let response = completion.borrow().clone();
                if let Some(response) = response { task.await.map_err(|error|ExtensionFailure::new(error.to_string()))?; return Ok(result(variant, &request, &response)); }
                completion.changed().await.map_err(|_|ExtensionFailure::new("Question owner disappeared"))?;
            }
        })
    })) { std::panic::panic_any(error); }
}
