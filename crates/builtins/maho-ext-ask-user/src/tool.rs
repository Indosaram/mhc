use crate::{
    family::tool_name,
    format::{format_result_details, format_result_text, format_user_message},
    notify::*,
    pending::PendingTimer,
    registry::*,
    schema::{AskUserVariant, claude_params, codex_params, to_canonical},
};
use maho_ai::types::TextContent;
use maho_ext_api::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct AskUserState {
    pub timed_out: bool,
    pub unavailable: bool,
}
#[derive(Clone, Copy)]
pub(crate) struct QuestionExecution {
    pub variant: AskUserVariant,
    pub resuming: bool,
    pub required: bool,
}
fn unavailable(request: &QuestionRequest) -> QuestionResponse {
    unavailable_response(
        request
            .questions
            .iter()
            .map(|question| question.id.clone())
            .collect(),
    )
}
fn unavailable_response(unanswered: Vec<String>) -> QuestionResponse {
    QuestionResponse {
        status: QuestionStatus::Unavailable,
        answers: Default::default(),
        comment: None,
        unanswered,
        auto_resolved_after_ms: None,
        cancel_origin: None,
        resolved_by: None,
    }
}
fn result(
    variant: AskUserVariant,
    request: &QuestionRequest,
    response: &QuestionResponse,
    required: bool,
) -> AgentToolResult {
    let text = if required
        && !matches!(
            response.status,
            QuestionStatus::Answered | QuestionStatus::CommentSubmitted
        ) {
        crate::format::format_required_no_answer(response, &request.questions)
    } else if response.status == QuestionStatus::Cancelled
        && !maho_ext_api::shows_partial_answers(response)
    {
        response
            .comment
            .clone()
            .unwrap_or_else(|| format_result_text(response, &request.questions))
    } else {
        format_result_text(response, &request.questions)
    };
    AgentToolResult {
        content: vec![ContentBlock::Text(TextContent {
            text,
            audience: None,
            text_signature: None,
        })],
        details: format_result_details(variant, response, &request.questions),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}
fn emit_wake(bus: &EventBus, session: &str) {
    let entries = get_pending_questions(session)
        .into_iter()
        .filter(|entry| !entry.request.wait_for_answer)
        .collect::<Vec<_>>();
    bus.emit("wake_source_state",&json!({"source":"ask-user","activeCount":entries.len(),"items":entries.iter().map(|entry|json!({"id":entry.request.request_id,"deadlineAtMs":(entry.deadline_at_ms)(),"description":entry.request.questions.iter().map(|question|question.header.as_str()).collect::<Vec<_>>().join(", ")})).collect::<Vec<_>>()}));
}
/// The framed user message `deliverAnswer` sends for a settled async (or resumed) question. A
/// cancelled question stays silent unless the user chose the cancel (or its UI failed) after
/// answering part of it; then those answers go out (omo-desktop-app#4042).
pub(crate) fn answer_delivery(
    request: &QuestionRequest,
    response: &QuestionResponse,
    resuming: bool,
) -> Option<String> {
    if request.wait_for_answer && !resuming {
        return None;
    }
    if response.status == QuestionStatus::Cancelled
        && !maho_ext_api::shows_partial_answers(response)
    {
        return None;
    }
    Some(format_user_message(
        response,
        &request.request_id,
        &request.questions,
    ))
}
fn publish(
    owner: QuestionOwner,
    request: &QuestionRequest,
    response: &QuestionResponse,
    variant: AskUserVariant,
    resuming: bool,
) {
    emit_closed(&owner.sender.events, &request.request_id, response);
    owner.sender.events.emit(
        "herdr:blocked",
        &json!({"active":false,"id":request.request_id}),
    );
    if !request.wait_for_answer
        && let Err(error) = owner.sender.append_entry(ASK_USER_SETTLEMENT_ENTRY, Some(json!({"requestId":request.request_id,"status":crate::format::status_name(response.status)}))) { owner.context.ui.notify(&error.message, NotificationType::Error); }
    if let Some(text) = answer_delivery(request, response, resuming) {
        let content = UserMessageContent::Text(text);
        let options = SendUserMessageOptions {
            deliver_as: Some(if owner.context.is_idle() {
                StreamingBehavior::FollowUp
            } else {
                StreamingBehavior::Steer
            }),
            expand_prompt_templates: false,
        };
        // Release held wakes once, before delivery, so the answer is admitted as attended input.
        if matches!(
            response.status,
            QuestionStatus::Answered | QuestionStatus::CommentSubmitted
        ) && let Some(release) = owner
            .context
            .session_manager
            .extension_context_actions()
            .and_then(|actions| actions.question_input())
        {
            release();
        }
        if let Err(error) = owner.sender.send_user_message(content, options) {
            owner
                .context
                .ui
                .notify(&error.message, NotificationType::Error);
        }
    }
    emit_notification(
        &owner.sender.events,
        &owner.context,
        request,
        response,
        variant,
    );
}
/// Mark a cancel's origin unless an earlier cancel already did; other statuses carry none.
fn with_origin(mut response: QuestionResponse, origin: QuestionCancelOrigin) -> QuestionResponse {
    if response.status == QuestionStatus::Cancelled && response.cancel_origin.is_none() {
        response.cancel_origin = Some(origin);
    }
    response
}
pub fn register_tool(
    api: &mut ExtensionApi,
    variant: AskUserVariant,
    state: Arc<Mutex<AskUserState>>,
    sender: Arc<ExtensionApi>,
) {
    let params = match variant {
        AskUserVariant::Codex => codex_params(),
        AskUserVariant::Claude => claude_params(),
    };
    let mut definition = ToolDefinition::new(
        tool_name(variant),
        "Ask a material question; choose explicitly whether to wait or receive the answer later. When the user's instructions (in the conversation or their instruction files) say not to ask, or not to ask with choices, do not call it: take the safe default, keep working, and say what you chose.",
        params,
        Arc::new(|_| {
            Box::pin(async { Err(ToolError::Message("Extension context required".into())) })
        }),
    );
    definition.label = "Ask user".into();
    definition.workspace_effect = Some(maho_tools::definition::ToolWorkspaceEffect::Read);
    definition.prompt_snippet = Some(
        "Ask a material question, explicitly choosing whether to wait or receive the answer later."
            .into(),
    );
    definition.allow_lazy_activation = Some(false);
    definition.prepare_arguments = Some(Arc::new(move |args| {
        to_canonical(variant, &args, String::new(), None).map_err(ToolError::Message)?;
        Ok(args)
    }));
    if let Err(error) = api.register_tool_with_extension_context(
        definition,
        Arc::new(move |id, args, signal, _, ctx| {
            let state = state.clone();
            let sender = sender.clone();
            Box::pin(async move {
                let settings = ctx.get_ask_user_settings()?;
                let timeout = (settings.timeout_minutes * 60_000.0) as u64;
                let request = match to_canonical(variant, &args, id.into(), Some(timeout)) {
                    Ok(request) => request,
                    Err(message) => {
                        emit_closed(&sender.events, id, &unavailable_response(Vec::new()));
                        return Ok(AgentToolResult {
                            content: vec![ContentBlock::Text(TextContent {
                                text: message,
                                audience: None,
                                text_signature: None,
                            })],
                            details: json!({"status":"unavailable"}),
                            structured_content: None,
                            usage: None,
                            nested_calls: None,
                            added_tool_names: None,
                            terminate: None,
                            is_error: None,
                        });
                    }
                };
                let required =
                    args.get("required").and_then(serde_json::Value::as_bool) == Some(true);
                let (timed_out, unavailable_now) = {
                    let current = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    (
                        current.timed_out,
                        current.unavailable
                            || !settings.enabled
                            || sender.get_flag("no-ask-user") == Some(FlagValue::Boolean(true))
                            || matches!(ctx.mode, ExtensionMode::Print | ExtensionMode::Json),
                    )
                };
                if timed_out {
                    emit_closed(&sender.events, &request.request_id, &unavailable(&request));
                    let response = result(variant, &request, &unavailable(&request), required);
                    return Ok(response);
                }
                if unavailable_now {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .unavailable = true;
                    let actions = sender.runtime.session_actions()?;
                    actions.set_active_tools(
                        actions
                            .get_active_tools()?
                            .into_iter()
                            .filter(|name| !crate::family::TOOL_NAMES.contains(&name.as_str()))
                            .collect(),
                    )?;
                    emit_closed(&sender.events, &request.request_id, &unavailable(&request));
                    return Ok(result(variant, &request, &unavailable(&request), required));
                }
                start_question(
                    sender,
                    ctx.clone(),
                    request,
                    signal,
                    state,
                    QuestionExecution {
                        variant,
                        resuming: false,
                        required,
                    },
                )
                .await
            })
        }),
    ) {
        std::panic::panic_any(error);
    }
    api.registered.tool_renderers.insert(
        tool_name(variant).into(),
        Arc::new(crate::render::renderers()),
    );
}

pub(crate) async fn start_question(
    sender: Arc<ExtensionApi>,
    ctx: ExtensionContext,
    request: QuestionRequest,
    signal: Option<maho_ai::utils::abort::AbortSignal>,
    state: Arc<Mutex<AskUserState>>,
    execution: QuestionExecution,
) -> Result<AgentToolResult, ExtensionFailure> {
    let QuestionExecution {
        variant,
        resuming,
        required,
    } = execution;
    let id = request.request_id.clone();
    let session = ctx.session_manager.session_id().to_owned();
    if let Some(existing) = get_pending_questions(&session)
        .into_iter()
        .find(|entry| entry.request.request_id == id)
    {
        let mut completion = existing.completion.clone();
        loop {
            if let Some(response) = completion.borrow().clone() {
                return Ok(result(variant, &request, &response, required));
            }
            completion
                .changed()
                .await
                .map_err(|_| ExtensionFailure::new("Question owner disappeared"))?;
        }
    }
    sender.append_entry(ASK_USER_QUESTION_ENTRY, Some(json!({"requestId":id,"headers":request.questions.iter().map(|question|&question.header).collect::<Vec<_>>()})))?;
    let (settled, mut completion) = tokio::sync::watch::channel(None::<QuestionResponse>);
    let (terminal, terminal_response) = tokio::sync::watch::channel(None::<QuestionResponse>);
    let (owner, owners) = tokio::sync::watch::channel(Some(QuestionOwner {
        sender: sender.clone(),
        context: ctx.clone(),
        state: state.clone(),
    }));
    let (attached, mut attachments) = tokio::sync::watch::channel(true);
    let (publishing, _) = tokio::sync::watch::channel(false);
    let publication = Arc::new(Mutex::new(publishing));
    let cancel_signal = AbortSignal::default();
    let callback = terminal.clone();
    let timer = Arc::new(PendingTimer::new(
        request.clone(),
        Arc::new(move |response| {
            callback.send_if_modified(|value| {
                if value.is_some() {
                    false
                } else {
                    *value = Some(response);
                    true
                }
            });
        }),
    ));
    let cancelled = terminal.clone();
    let pending = timer.clone();
    let cancel = cancel_signal.clone();
    let deadline = timer.clone();
    let attachment = attached.subscribe();
    // senpi rebinds `notify` to each new owner's ctx; the first ctx's UI is kept only while detached.
    let loss_owner = owner.subscribe();
    let loss_ui = ctx.ui.clone();
    register_pending_question(
        &session,
        Arc::new(PendingQuestionEntry {
            request: request.clone(),
            completion: completion.clone(),
            owner,
            attached,
            publication: publication.clone(),
            deadline_at_ms: Arc::new(move || deadline.absolute_deadline_at_ms()),
            cancel: Arc::new(move |origin, message, report_detached_loss| {
                let lost = report_detached_loss && !*attachment.borrow();
                let mut response = pending.cancel(if lost {
                    QuestionStatus::OrphanedAfterRestart
                } else {
                    QuestionStatus::Cancelled
                });
                if let Some(message) = &message {
                    response.comment = Some(message.clone());
                }
                if response.status == QuestionStatus::Cancelled {
                    response.cancel_origin = Some(origin);
                }
                cancelled.send_if_modified(|value| {
                    if value.is_some() {
                        false
                    } else {
                        *value = Some(response);
                        true
                    }
                });
                cancel.abort();
                if lost {
                    let ui = loss_owner
                        .borrow()
                        .as_ref()
                        .map_or_else(|| loss_ui.clone(), |owner| owner.context.ui.clone());
                    ui.notify(
                        message
                            .as_deref()
                            .unwrap_or("The pending question could not be restored."),
                        NotificationType::Error,
                    );
                }
            }),
        }),
    );
    if !request.wait_for_answer {
        emit_wake(&sender.events, &session);
    }
    emit_asked(&sender.events, &ctx, &request, variant);
    sender.events.emit("herdr:blocked", &json!({"active":true,"id":id,"label":request.questions.first().map(|q|format!("{} - {}",q.header,q.question)).unwrap_or_else(||"Question".into())}));
    let owner_request = request.clone();
    let work_completion = terminal_response;
    let context_signal = if resuming { ctx.signal.clone() } else { None };
    let settle_state = state.clone();
    let task = tokio::spawn(async move {
        let mut completed = work_completion;
        let mut reattached = false;
        let response = loop {
            // The UI opens only while attached; a re-bound owner stays closed until SessionStart
            // re-attaches it after the disabled check (senpi rebind -> sync -> reattach).
            let current = if *attachments.borrow_and_update() {
                owners.borrow().clone()
            } else {
                None
            };
            let progress = timer.clone();
            let progress_request = owner_request.clone();
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
                        let bus = owner.sender.events.clone();
                        let session = owner.context.session_manager.session_id().to_owned();
                        let progress_signal = ui_signal.clone();
                        owner
                            .context
                            .ui
                            .question(
                                request.clone(),
                                QuestionOptions {
                                    dialog: ExtensionUiDialogOptions {
                                        signal: Some(ui_signal),
                                        timeout_ms: Some(remaining_ms),
                                        ..Default::default()
                                    },
                                    on_progress: Some(Arc::new(move |draft| {
                                        if progress_signal.is_aborted() {
                                            return;
                                        }
                                        progress.progress(draft);
                                        if !progress_request.wait_for_answer {
                                            emit_wake(&bus, &session);
                                        }
                                    })),
                                    // A resumed question delivers its answer as a user message, like an async one.
                                    deliver: if request.wait_for_answer && !resuming {
                                        QuestionDelivery::ToolResult
                                    } else {
                                        QuestionDelivery::UserMessage
                                    },
                                    hard_deadline_at_ms: Some(hard_deadline_at_ms),
                                    get_deadline_at_ms: Some(Arc::new(move || {
                                        deadline.absolute_deadline_at_ms()
                                    })),
                                    initial_draft: Some(initial_draft),
                                },
                            )
                            .await
                    }
                    None => std::future::pending().await,
                }
            };
            let response = tokio::select! {
                biased;
                update = attachments.changed() => { attachment_signal.abort(); reattached=true;if update.is_err() { Some(timer.cancel(QuestionStatus::Cancelled)) } else { None } },
                () = async { if let Some(signal) = &signal { signal.cancelled().await; } else { std::future::pending::<()>().await; } } => Some(with_origin(timer.cancel(QuestionStatus::Cancelled), QuestionCancelOrigin::TurnAbort)),
                () = async { if let Some(signal) = &context_signal { signal.cancelled().await; } else { std::future::pending::<()>().await; } } => Some(with_origin(timer.cancel(QuestionStatus::Cancelled), QuestionCancelOrigin::TurnAbort)),
                () = async { loop { if completed.borrow().is_some() || completed.changed().await.is_err() { break; } } } => Some(completed.borrow().clone().unwrap_or_else(|| timer.cancel(QuestionStatus::Cancelled))),
                response = question => Some(match response {
                    // A cancel without answers keeps the draft the UI reported as progress; a cancel
                    // carrying answers updates it. `shows_partial_answers` alone decides delivery.
                    Ok(response) if response.status == QuestionStatus::Cancelled => {
                        if !response.answers.is_empty() || response.comment.is_some() {
                            timer.touch(Some((response.answers.clone(), response.comment.clone())));
                        }
                        let mut cancelled = timer.cancel(QuestionStatus::Cancelled);
                        if response.cancel_origin.is_some() { cancelled.cancel_origin = response.cancel_origin; }
                        cancelled
                    }
                    Ok(response) => response, Err(error) => {
                    let message=format!("Question UI failed: {error}");
                    let mut response=with_origin(timer.cancel(if resuming||reattached{QuestionStatus::OrphanedAfterRestart}else{QuestionStatus::Cancelled}),QuestionCancelOrigin::UiFailure);response.comment=Some(message.clone());
                    let owner=owners.borrow().clone();if let Some(owner)=owner{owner.context.ui.notify(&message,NotificationType::Error);}
                    response
                } }),
            };
            attachment_signal.abort();
            if let Some(response) = response {
                break response;
            }
        };
        if response.status == QuestionStatus::TimedOut {
            settle_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .timed_out = true;
        }
        match response.status {
            QuestionStatus::Answered | QuestionStatus::CommentSubmitted => {
                timer.submit(response.answers.clone(), response.comment.clone());
                timer.cancel(QuestionStatus::Cancelled);
            }
            other => {
                timer.cancel(other);
            }
        }
        timer.settle().await;
        cancel_signal.abort();
        let owner = select_publication(&owners, &publication, || {
            let request = owner_request.clone();
            let outcome = response.clone();
            queue_outcome(
                &session,
                Box::new(move |owner| publish(owner, &request, &outcome, variant, resuming)),
            );
        });
        if let Some(owner) = owner {
            publish(owner, &owner_request, &response, variant, resuming);
        }
        publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .send_replace(false);
        unregister_pending_question(&session, &owner_request.request_id);
        if !owner_request.wait_for_answer {
            let owner = owners.borrow().clone();
            if let Some(owner) = owner {
                emit_wake(&owner.sender.events, &session);
            }
        }
        settled.send_replace(Some(response));
    });
    if resuming || !request.wait_for_answer {
        return Ok(AgentToolResult {
            content: vec![ContentBlock::Text(TextContent {
                text: "Question accepted; the answer will arrive as a user message.".into(),
                audience: None,
                text_signature: None,
            })],
            details: json!({"accepted":true,"requestId":id,"status":"pending"}),
            structured_content: None,
            usage: None,
            nested_calls: None,
            added_tool_names: None,
            terminate: None,
            is_error: None,
        });
    }
    loop {
        let response = completion.borrow().clone();
        if let Some(response) = response {
            task.await
                .map_err(|error| ExtensionFailure::new(error.to_string()))?;
            return Ok(result(variant, &request, &response, required));
        }
        completion
            .changed()
            .await
            .map_err(|_| ExtensionFailure::new("Question owner disappeared"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(wait_for_answer: bool) -> QuestionRequest {
        let question = |id: &str, header: &str, labels: [&str; 2]| Question {
            id: id.into(),
            header: header.into(),
            question: "Which?".into(),
            options: labels
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).into(),
                    description: None,
                })
                .collect(),
            multi_select: false,
        };
        QuestionRequest {
            request_id: "async-q".into(),
            questions: vec![
                question("q1", "Auth", ["A", "B"]),
                question("q2", "Library", ["C", "D"]),
            ],
            wait_for_answer,
            timeout_ms: 60_000,
        }
    }
    fn cancelled(origin: Option<QuestionCancelOrigin>, comment: Option<&str>) -> QuestionResponse {
        QuestionResponse {
            status: QuestionStatus::Cancelled,
            answers: [(
                "q1".to_owned(),
                QuestionAnswer {
                    selected: vec!["A".into()],
                    text: None,
                },
            )]
            .into(),
            comment: comment.map(str::to_owned),
            unanswered: vec!["q2".into()],
            auto_resolved_after_ms: None,
            cancel_origin: origin,
            resolved_by: None,
        }
    }
    // senpi ask-user-cancel-origin.test.ts: only a cancel the user chose, or a failing question UI,
    // shows the model the answers given before it; a lifecycle cancel stays silent.
    #[test]
    fn partial_answers_reach_the_model_only_for_user_and_ui_failure_cancels() {
        for origin in [
            None,
            Some(QuestionCancelOrigin::TurnAbort),
            Some(QuestionCancelOrigin::SessionClosed),
            Some(QuestionCancelOrigin::AskUserDisabled),
        ] {
            assert_eq!(
                answer_delivery(&request(false), &cancelled(origin, None), false),
                None
            );
        }
        assert_eq!(
            answer_delivery(
                &request(false),
                &cancelled(Some(QuestionCancelOrigin::User), None),
                false
            )
            .as_deref(),
            Some(
                "[Answer to question async-q]\nThe user cancelled the question after answering part of it.\nAuth: A\nUnanswered: Library"
            )
        );
        assert_eq!(
            answer_delivery(
                &request(false),
                &cancelled(
                    Some(QuestionCancelOrigin::UiFailure),
                    Some("Question UI failed: boom")
                ),
                false
            )
            .as_deref(),
            Some(
                "[Answer to question async-q]\nQuestion UI failed: boom\nThe question was cancelled after part of it was answered.\nAuth: A\nUnanswered: Library"
            )
        );
        let user = cancelled(Some(QuestionCancelOrigin::User), None);
        assert_eq!(answer_delivery(&request(true), &user, false), None);
        assert!(answer_delivery(&request(true), &user, true).is_some());
    }
    // A waiting question's tool result shows the partial answers instead of the bare comment.
    #[test]
    fn waiting_result_formats_partial_answers_instead_of_the_comment() {
        let response = cancelled(
            Some(QuestionCancelOrigin::UiFailure),
            Some("Question UI failed: boom"),
        );
        let ContentBlock::Text(text) =
            &result(AskUserVariant::Claude, &request(true), &response, false).content[0]
        else {
            panic!("text result")
        };
        assert!(text.text.contains("Auth: A"), "{}", text.text);
        let lifecycle = cancelled(
            Some(QuestionCancelOrigin::SessionClosed),
            Some("The pending question was cancelled because the session closed."),
        );
        let ContentBlock::Text(text) =
            &result(AskUserVariant::Claude, &request(true), &lifecycle, false).content[0]
        else {
            panic!("text result")
        };
        assert_eq!(
            text.text,
            "The pending question was cancelled because the session closed."
        );
    }
}
