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
    let text=if response.status==QuestionStatus::Cancelled{response.comment.clone().unwrap_or_else(||format_result_text(response,&request.questions))}else{format_result_text(response,&request.questions)};
    AgentToolResult { content: vec![ContentBlock::Text(TextContent { text, audience: None, text_signature: None })], details: format_result_details(variant, response, &request.questions), usage: None, added_tool_names: None, terminate: None, is_error: None }
}
fn emit_wake(bus:&EventBus,session:&str){
    let entries=get_pending_questions(session).into_iter().filter(|entry|!entry.request.wait_for_answer).collect::<Vec<_>>();
    bus.emit("wake_source_state",&json!({"source":"ask-user","activeCount":entries.len(),"items":entries.iter().map(|entry|json!({"id":entry.request.request_id,"deadlineAtMs":(entry.deadline_at_ms)(),"description":entry.request.questions.iter().map(|question|question.header.as_str()).collect::<Vec<_>>().join(", ")})).collect::<Vec<_>>()}));
}
fn publish(owner: QuestionOwner, request: &QuestionRequest, response: &QuestionResponse, variant: AskUserVariant, resuming: bool) {
    owner.sender.events.emit("herdr:blocked", &json!({"active":false,"id":request.request_id}));
    if !request.wait_for_answer
        && let Err(error) = owner.sender.append_entry(ASK_USER_SETTLEMENT_ENTRY, Some(json!({"requestId":request.request_id,"status":crate::format::status_name(response.status)}))) { owner.context.ui.notify(&error.message, NotificationType::Error); }
    if (!request.wait_for_answer || resuming)
        && response.status != QuestionStatus::Cancelled
        && let Err(error) = owner.sender.send_user_message(UserMessageContent::Text(format_user_message(response, &request.request_id, &request.questions)), SendUserMessageOptions { deliver_as: Some(if owner.context.is_idle() { StreamingBehavior::FollowUp } else { StreamingBehavior::Steer }), expand_prompt_templates: false }) {
        owner.context.ui.notify(&error.message, NotificationType::Error);
    }
    emit_notification(&owner.sender.events, &owner.context, request, response, variant);
}
pub fn register_tool(api: &mut ExtensionApi, variant: AskUserVariant, state: Arc<Mutex<AskUserState>>, sender: Arc<ExtensionApi>) {
    let params = match variant { AskUserVariant::Codex => codex_params(), AskUserVariant::Claude => claude_params() };
    let mut definition = ToolDefinition::new(tool_name(variant), "Ask a material question; choose explicitly whether to wait or receive the answer later.", params, Arc::new(|_| Box::pin(async { Err(ToolError::Message("Extension context required".into())) })));
    definition.label = "Ask user".into();
    definition.prompt_snippet=Some("Ask a material question, explicitly choosing whether to wait or receive the answer later.".into());
    definition.allow_lazy_activation = Some(false);
    definition.prepare_arguments = Some(Arc::new(move |args| { to_canonical(variant, &args, String::new(), None).map_err(ToolError::Message)?; Ok(args) }));
    if let Err(error) = api.register_tool_with_extension_context(definition, Arc::new(move |id, args, signal, _, ctx| {
        let state = state.clone(); let sender = sender.clone();
        Box::pin(async move {
            let settings = ctx.get_ask_user_settings()?;
            let timeout = (settings.timeout_minutes * 60_000.0) as u64;
            let request = match to_canonical(variant, &args, id.into(), Some(timeout)){
                Ok(request)=>request,
                Err(message)=>return Ok(AgentToolResult{content:vec![ContentBlock::Text(TextContent{text:message,audience:None,text_signature:None})],details:json!({"status":"unavailable"}),usage:None,added_tool_names:None,terminate:None,is_error:None}),
            };
            let (timed_out, unavailable_now) = {
                let current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (current.timed_out,current.unavailable||!settings.enabled||sender.get_flag("no-ask-user")==Some(FlagValue::Boolean(true))||matches!(ctx.mode,ExtensionMode::Print|ExtensionMode::Json))
            };
            if timed_out {
                let mut response=result(variant,&request,&unavailable(&request));
                response.content=vec![ContentBlock::Text(TextContent{text:"The user did not answer the previous question this turn; continue without asking again.".into(),audience:None,text_signature:None})];
                return Ok(response);
            }
            if unavailable_now {
                state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unavailable=true;
                let actions=sender.runtime.session_actions()?;
                actions.set_active_tools(actions.get_active_tools()?.into_iter().filter(|name|!crate::family::TOOL_NAMES.contains(&name.as_str())).collect())?;
                return Ok(result(variant,&request,&unavailable(&request)));
            }
            start_question(sender, ctx.clone(), request, signal, state, variant, false).await
        })
    })) { std::panic::panic_any(error); }
    api.registered.tool_renderers.insert(tool_name(variant).into(),Arc::new(crate::render::renderers()));
}

pub(crate) async fn start_question(sender: Arc<ExtensionApi>, ctx: ExtensionContext, request: QuestionRequest, signal: Option<maho_ai::utils::abort::AbortSignal>, state: Arc<Mutex<AskUserState>>, variant: AskUserVariant, resuming: bool) -> Result<AgentToolResult, ExtensionFailure> {
            let id = request.request_id.clone();
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
            let deadline=timer.clone();
            register_pending_question(&session, Arc::new(PendingQuestionEntry { request: request.clone(), completion: completion.clone(), owner, publication: publication.clone(), deadline_at_ms:Arc::new(move||deadline.absolute_deadline_at_ms()), cancel: Arc::new(move |reason| { let response = pending.cancel(reason); cancelled.send_if_modified(|value| { if value.is_some() { false } else { *value = Some(response); true } }); cancel.abort(); }) }));
            if !request.wait_for_answer{emit_wake(&sender.events,&session);}
            emit_asked(&sender.events, &ctx, &request, variant);
            sender.events.emit("herdr:blocked", &json!({"active":true,"id":id,"label":request.questions.first().map(|q|format!("{} — {}",q.header,q.question)).unwrap_or_default()}));
            let owner_request = request.clone();
            let work_completion = terminal_response;
            let context_signal = if resuming { ctx.signal.clone() } else { None };
            let settle_state = state.clone();
            let task = tokio::spawn(async move {
                let mut completed = work_completion;
                let mut reattached=false;
                let response = loop {
                    let current = owners.borrow_and_update().clone();
                    let progress = timer.clone();
                    let progress_request=owner_request.clone();
                    let remaining_ms = timer.remaining_ms();
                    let attachment_signal = AbortSignal::default();
                    let ui_signal = attachment_signal.clone();
                    let request = owner_request.clone();
                    let deadline = timer.clone();
                    let initial_draft = timer.initial_draft();
                    let hard_deadline_at_ms = timer.absolute_hard_deadline_at_ms();
                    let question = async move {
                        match current {
                            Some(owner) => {
                                let bus=owner.sender.events.clone();let session=owner.context.session_manager.session_id().to_owned();let progress_signal=ui_signal.clone();
                                owner.context.ui.question(request.clone(), QuestionOptions { dialog: ExtensionUiDialogOptions { signal: Some(ui_signal), timeout_ms: Some(remaining_ms) }, on_progress: Some(Arc::new(move |draft| {
                                    if progress_signal.is_aborted(){return;}
                                    progress.progress(draft);if !progress_request.wait_for_answer{emit_wake(&bus,&session);}
                                })), deliver: if request.wait_for_answer { QuestionDelivery::ToolResult } else { QuestionDelivery::UserMessage }, hard_deadline_at_ms: Some(hard_deadline_at_ms), get_deadline_at_ms: Some(Arc::new(move || deadline.absolute_deadline_at_ms())), initial_draft: Some(initial_draft) }).await
                            },
                            None => std::future::pending().await,
                        }
                    };
                    let response = tokio::select! {
                        biased;
                        update = owners.changed() => { attachment_signal.abort(); reattached=true;if update.is_err() { Some(timer.cancel(QuestionStatus::Cancelled)) } else { None } },
                        () = async { if let Some(signal) = &signal { signal.cancelled().await; } else { std::future::pending::<()>().await; } } => Some(timer.cancel(QuestionStatus::Cancelled)),
                        () = async { if let Some(signal) = &context_signal { signal.cancelled().await; } else { std::future::pending::<()>().await; } } => Some(timer.cancel(QuestionStatus::Cancelled)),
                        () = async { loop { if completed.borrow().is_some() || completed.changed().await.is_err() { break; } } } => Some(completed.borrow().clone().unwrap_or_else(|| timer.cancel(QuestionStatus::Cancelled))),
                        response = question => Some(match response { Ok(response) => response, Err(error) => {
                            let message=format!("Question UI failed: {error}");
                            let mut response=timer.cancel(if resuming||reattached{QuestionStatus::OrphanedAfterRestart}else{QuestionStatus::Cancelled});response.comment=Some(message.clone());
                            let owner=owners.borrow().clone();if let Some(owner)=owner{owner.context.ui.notify(&message,NotificationType::Error);}
                            response
                        } }),
                    };
                    attachment_signal.abort();
                    if let Some(response) = response { break response; }
                };
                if response.status == QuestionStatus::TimedOut { settle_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).timed_out = true; }
                match response.status {
                    QuestionStatus::Answered | QuestionStatus::CommentSubmitted => {
                        timer.submit(response.answers.clone(), response.comment.clone());
                        timer.cancel(QuestionStatus::Cancelled);
                    },
                    other => { timer.cancel(other); },
                }
                timer.settle().await;
                cancel_signal.abort();
                let owner = select_publication(&owners, &publication, || {
                    let request = owner_request.clone(); let outcome = response.clone();
                    queue_outcome(&session, Box::new(move |owner| publish(owner, &request, &outcome, variant, resuming)));
                });
                if let Some(owner) = owner { publish(owner, &owner_request, &response, variant, resuming); }
                publication.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send_replace(false);
                unregister_pending_question(&session, &owner_request.request_id);
                if !owner_request.wait_for_answer{let owner=owners.borrow().clone();if let Some(owner)=owner{emit_wake(&owner.sender.events,&session);}}
                settled.send_replace(Some(response));
            });
            if resuming || !request.wait_for_answer { return Ok(AgentToolResult { content: vec![ContentBlock::Text(TextContent { text:"Question accepted; the answer will arrive as a user message.".into(), audience:None, text_signature:None })], details:json!({"accepted":true,"requestId":id,"status":"pending"}), usage:None,added_tool_names:None,terminate:None,is_error:None }); }
            loop {
                let response = completion.borrow().clone();
                if let Some(response) = response { task.await.map_err(|error|ExtensionFailure::new(error.to_string()))?; return Ok(result(variant, &request, &response)); }
                completion.changed().await.map_err(|_|ExtensionFailure::new("Question owner disappeared"))?;
            }
}
