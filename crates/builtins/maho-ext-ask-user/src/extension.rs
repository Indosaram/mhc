use crate::{family::{TOOL_NAMES, pick_variant, tool_name}, registry::{get_pending_questions, deliver_outcomes, QuestionOwner}, schema::AskUserVariant, tool::{AskUserState, register_tool}};
use maho_ext_api::*;
use std::sync::{Arc, Mutex};

pub struct AskUser;
impl Extension for AskUser {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag("no-ask-user", FlagType::Boolean { default: Some(false) }, Some("Disable the built-in question tool.".into()));
        api.register_command("answer", Some("Open the pending question".into()), None, Arc::new(|_, ctx| Box::pin(async move {
            if ctx.mode != ExtensionMode::Tui { ctx.ui.notify("/answer opens the pending question in the TUI; run maho in TUI mode to use it.", NotificationType::Info); }
            Ok(())
        })));
        let state = Arc::new(Mutex::new(AskUserState::default()));
        let sender = Arc::new(ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
        for variant in [AskUserVariant::Codex, AskUserVariant::Claude] { register_tool(api, variant, state.clone(), sender.clone()); }
        for kind in [EventKind::SessionStart, EventKind::ModelSelect] {
            let state = state.clone(); let sender = sender.clone();
            api.on(kind, Arc::new(move |event, ctx| {
                let state = state.clone(); let sender = sender.clone();
                Box::pin(async move {
                    if kind == EventKind::SessionStart {
                        *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = AskUserState::default();
                        let owner = QuestionOwner { sender: sender.clone(), context: ctx.clone(), state: state.clone() };
                        for entry in get_pending_questions(ctx.session_manager.session_id()) {
                            let _publication = entry.publication.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            entry.owner.send_replace(Some(owner.clone()));
                        }
                        deliver_outcomes(ctx.session_manager.session_id(), owner);
                        if matches!(event, ExtensionEvent::SessionStart(event) if matches!(event.reason, SessionReason::Resume | SessionReason::Reload)) {
                            crate::resume::resume_dangling_questions(sender.clone(),ctx).await?;
                        }
                    }
                    let actions = sender.runtime.session_actions()?;
                    let mut active = actions.get_active_tools()?.into_iter().filter(|name| !TOOL_NAMES.contains(&name.as_str())).collect::<Vec<_>>();
                    let disabled = !ctx.get_ask_user_settings()?.enabled || sender.runtime.get_flag("no-ask-user") == Some(FlagValue::Boolean(true));
                    if disabled {
                        cancel_pending(ctx.session_manager.session_id()).await?;
                    } else if !state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unavailable {
                        let model = match event { ExtensionEvent::ModelSelect(event) => Some(&event.model), _ => ctx.model.as_ref() };
                        active.push(tool_name(pick_variant(model)).into());
                    }
                    actions.set_active_tools(active)?;
                    Ok(EventResult::None)
                })
            }));
        }
        let reset = state.clone();
        api.on(EventKind::AgentEnd, Arc::new(move |_, _| { let reset = reset.clone(); Box::pin(async move { reset.lock().unwrap_or_else(std::sync::PoisonError::into_inner).timed_out = false; Ok(EventResult::None) }) }));
        api.on(EventKind::SessionShutdown, Arc::new(|event, ctx| Box::pin(async move {
            if matches!(event, ExtensionEvent::SessionShutdown(event) if event.reason == SessionReason::Reload) {
                for entry in get_pending_questions(ctx.session_manager.session_id()) {
                    let mut publishing = {
                        let publication = entry.publication.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        entry.owner.send_replace(None);
                        publication.subscribe()
                    };
                    loop {
                        if !*publishing.borrow() { break; }
                        publishing.changed().await.map_err(|_| ExtensionFailure::new("Question publication disappeared during reload"))?;
                    }
                }
                return Ok(EventResult::None);
            }
            cancel_pending(ctx.session_manager.session_id()).await?;
            Ok(EventResult::None)
        })));
    }
}

async fn cancel_pending(session: &str) -> Result<(), ExtensionFailure> {
    let entries = get_pending_questions(session);
    for entry in &entries { (entry.cancel)(QuestionStatus::Cancelled); }
    for entry in entries {
        let mut completion = entry.completion.clone();
        loop {
            if completion.borrow().is_some() { break; }
            completion.changed().await.map_err(|_| ExtensionFailure::new("Question owner disappeared before shutdown settlement"))?;
        }
    }
    Ok(())
}
