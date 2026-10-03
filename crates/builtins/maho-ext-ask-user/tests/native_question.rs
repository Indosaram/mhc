#[path = "native_question/context.rs"]
mod context;
use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, ExecuteToolOptions};
use maho_ext_api::*;
use maho_ext_host::{ExtensionRunner, loader::{NativeExtensionFactory, load_extensions}};
use maho_ext_ask_user::{AskUser, AskUserAskedEvent, AskUserSettledEvent, registry::{get_pending_questions, unregister_pending_question}};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct CaptureStart(Arc<Mutex<Option<ExtensionContext>>>);
impl Extension for CaptureStart {
    fn register(&self, api: &mut ExtensionApi) {
        let captured = self.0.clone();
        api.on(EventKind::SessionStart, Arc::new(move |_, ctx| {
            *captured.lock().expect("start context") = Some(ctx.clone());
            Box::pin(async { Ok(EventResult::None) })
        }));
    }
}

async fn scenario(cancel: bool, fail_append: bool, abort: bool, timeout: bool, reload: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    scenario_order(cancel, fail_append, abort, timeout, reload, false).await
}
async fn scenario_order(cancel: bool, fail_append: bool, abort: bool, timeout: bool, reload: bool, late_rebind: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    scenario_recovery(cancel, fail_append, abort, timeout, reload, late_rebind, false).await
}
async fn scenario_recovery(cancel: bool, fail_append: bool, abort: bool, timeout: bool, reload: bool, late_rebind: bool, recovering: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    scenario_ui_failure(cancel,fail_append,abort,timeout,reload,late_rebind,recovering,false).await
}
async fn scenario_ui_failure(cancel: bool, fail_append: bool, abort: bool, timeout: bool, reload: bool, late_rebind: bool, recovering: bool, fail_ui:bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let project = root.path();
    let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
    let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
    let streams = faux_streams(provider.core.clone());
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(project.join("models.json")), auth_path: Some(project.join("auth.json")),
        providers: Some(vec![provider.provider.clone()]), ..Default::default()
    });
    let cwd = project.to_string_lossy().into_owned();
    let mut session_manager = maho_core::session_manager::SessionManager::in_memory(&cwd, None, None);
    if recovering {
        session_manager.append_message(json!({"role":"assistant","content":[{"type":"toolCall","id":"recovered-question","name":"ask_user_question","arguments":{"waitForAnswer":true,"questions":[{"header":"Choice","question":"Pick?","multiSelect":false,"options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}]}}],"api":"faux","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":0}));
    }
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)))), ..Default::default()
        }),
        session_manager,
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), true),
        cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None,
        excluded_tool_names: None, base_tools_override: None, session_start_event: recovering.then_some(SessionStartEvent {reason:SessionReason::Resume,initial_model_provenance:None,previous_session_file:None}), auto_title_sessions: Some(false),
    })?;
    let loaded = load_extensions(vec![NativeExtensionFactory { path: "<ask-user-native>".into(), source_info: SourceInfo::default(), extension: Box::new(AskUser) }], project, ExtensionSessionProfile::default());
    let extension_runtime = loaded.runtime.clone();
    if !loaded.errors.is_empty() { return Err(format!("Factory errors: {:?}", loaded.errors).into()); }
    let (opened, mut opened_rx) = tokio::sync::mpsc::unbounded_channel();
    let (responses, response_rx) = tokio::sync::watch::channel(None);
    let mut responses=Some(responses);
    let ui = Arc::new(context::DecisionUi { opened, responses: response_rx });
    let mut event_context = context::create(&session, ui.clone());
    event_context.mode = ExtensionMode::Tui;
    let bus = loaded.events.clone();
    let settlements = Arc::new(Mutex::new(Vec::new()));
    let captured = settlements.clone();
    let _subscription = bus.on_native::<AskUserSettledEvent>("ask-user:settled", Arc::new(move |value| captured.lock().expect("settlements").push(value.clone())));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let captured = asked.clone();
    let _asked = bus.on_native::<AskUserAskedEvent>("ask-user:asked", Arc::new(move |value| captured.lock().expect("asked").push(value.clone())));
    let ferryx_asked = Arc::new(Mutex::new(Vec::new()));
    let captured = ferryx_asked.clone();
    let _ferryx_asked = bus.on("ask-user:asked", Arc::new(move |value| captured.lock().expect("ferryx asked").push(value.clone())));
    let legacy = Arc::new(Mutex::new(0));
    let captured = legacy.clone();
    let _legacy = bus.on("ask-user:settled", Arc::new(move |_| *captured.lock().expect("legacy") += 1));
    let runner = ExtensionRunner::new(loaded.extensions, loaded.runtime, loaded.events, event_context);
    session.set_extension_runner(runner).await;
    session.bind_extensions(ExtensionBindings { ui_context: Some(ui.clone() as Arc<dyn ExtensionUi>), mode: Some(ExtensionMode::Tui), ..Default::default() }).await;
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    let outcome = async {
    let args = json!({"wait_for_answer":false,"questions":[{"id":"choice","header":"Choice","question":"Pick?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}]});
    let tool = session.get_active_tool_names().into_iter().find(|name| ["request_user_input", "ask_user_question"].contains(&name.as_str())).ok_or("No active question tool")?;
    let args = if tool == "ask_user_question" {
        json!({"waitForAnswer":false,"questions":[{"header":"Choice","question":"Pick?","multiSelect":false,"options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}]})
    } else { args };
    if fail_append {
        extension_runtime.bind(Arc::new(FailingPersistence));
        let result = session.execute_tool(&tool, args, ExecuteToolOptions::default()).await;
        let pending = get_pending_questions(&session.session_id()).len();
        let notifications = settlements.lock().expect("settlements").len();
        let opened = opened_rx.try_recv().is_ok();
        let failed = result.as_ref().map_or(true, |result| result.is_error == Some(true));
        let asked_count = asked.lock().expect("asked").len();
        let ferryx_count = ferryx_asked.lock().expect("ferryx asked").len();
        if !failed || pending != 0 || notifications != 0 || asked_count != 0 || ferryx_count != 0 || opened {
            return Err(format!("failed setup receipt: failed={failed}, pending={pending}, settlements={notifications}, opened={opened}, result={result:?}").into());
        }
        return Ok(());
    }
    if !recovering {
        let result = session.execute_tool(&tool, args.clone(), ExecuteToolOptions { signal: Some(signal.clone()), ..Default::default() }).await?;
        if result.details["accepted"] != true { return Err(format!("Question not accepted: {result:?}").into()); }
    }
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), opened_rx.recv()).await?.ok_or("UI did not open")?;
    let pending = get_pending_questions(&session.session_id());
    if pending.len() != 1 { return Err(format!("Expected one pending question, got {}", pending.len()).into()); }
    if recovering {
        session.bind_extensions(ExtensionBindings {ui_context:Some(ui.clone() as Arc<dyn ExtensionUi>),mode:Some(ExtensionMode::Tui),..Default::default()}).await;
        let entries = session.with_session_manager(|manager|manager.entries());
        if request.request_id != "recovered-question" || entries.iter().filter(|entry|entry["customType"]=="ask-user:resumed" && entry["data"]["toolCallId"]=="recovered-question").count()!=1 {
            return Err("Recovery did not mark original call exactly once".into());
        }
    }
    let initial_owner = pending[0].owner.borrow().clone().ok_or("Missing owner")?;
    {
        let events = asked.lock().expect("asked");
        if events.len() != 1 { return Err(format!("Expected one native asked event, got {}", events.len()).into()); }
        let event = &events[0];
        if !Arc::ptr_eq(&event.ctx.session_manager, &initial_owner.context.session_manager)
            || !Arc::ptr_eq(&event.ctx.model_registry, &initial_owner.context.model_registry)
            || !Arc::ptr_eq(&event.ctx.ui, &initial_owner.context.ui)
            || event.request != request {
            return Err("Asked event lost owning context or complete request".into());
        }
        let bridge = ferryx_asked.lock().expect("ferryx asked");
        if bridge.len() != 1 || bridge[0]["request"]["requestId"] != request.request_id
            || bridge[0]["request"]["waitForAnswer"] != request.wait_for_answer
            || bridge[0]["request"]["questions"][0]["header"] != request.questions[0].header {
            return Err("Required Ferryx JSON asked bridge missing or duplicated".into());
        }
    }
    let callback_owner = pending[0].owner.clone();
    let _reentrant_detach = bus.on_native::<AskUserSettledEvent>("ask-user:settled",Arc::new(move |_| {
        callback_owner.send_replace(None);
    }));
    let mut completion = pending[0].completion.clone();
    if reload {
        if !late_rebind {
            responses.as_ref().expect("response sender").send_replace(Some(QuestionResponse {status:QuestionStatus::Answered,answers:[(request.questions[0].id.clone(),QuestionAnswer {selected:vec!["A".into()],text:None})].into(),comment:None,unanswered:vec![],auto_resolved_after_ms:None}));
        }
        session.emit_session_shutdown(SessionReason::Reload).await;
        if pending[0].owner.borrow().is_some() { return Err("Reload did not detach owner".into()); }
        extension_runtime.invalidate("old question runner invalidated");
    }
    if fail_ui {
        drop(responses.take());
    } else if timeout {
        tokio::time::advance(std::time::Duration::from_secs(30 * 60)).await;
    } else if abort {
        controller.abort(None);
    } else if cancel {
        session.emit_session_shutdown(SessionReason::Quit).await;
        if !get_pending_questions(&session.session_id()).is_empty() { return Err("Shutdown left pending question".into()); }
    } else if !late_rebind {
        responses.as_ref().expect("response sender").send_replace(Some(QuestionResponse {status:QuestionStatus::Answered, answers:[(request.questions[0].id.clone(),QuestionAnswer {selected:vec!["A".into()],text:None})].into(),comment:None,unanswered:vec![],auto_resolved_after_ms:None}));
    }
    if !late_rebind { tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop { if completion.borrow().is_some() { break; } completion.changed().await?; }
        Ok::<(), tokio::sync::watch::error::RecvError>(())
    }).await??; }
    let replacement_context = Arc::new(Mutex::new(None));
    let _new_subscription = if reload {
        if !settlements.lock().expect("settlements").is_empty() { return Err("Detached timeout notified old runner".into()); }
        let loaded = load_extensions(vec![NativeExtensionFactory { path:"<capture-reloaded-context>".into(),source_info:SourceInfo::default(),extension:Box::new(CaptureStart(replacement_context.clone())) }, NativeExtensionFactory { path:"<ask-user-reloaded>".into(),source_info:SourceInfo::default(),extension:Box::new(AskUser) }],project,ExtensionSessionProfile::default());
        if !loaded.errors.is_empty() { return Err(format!("Reload factory errors: {:?}",loaded.errors).into()); }
        let captured = settlements.clone();
        let subscription = loaded.events.on_native::<AskUserSettledEvent>("ask-user:settled",Arc::new(move |value|captured.lock().expect("settlements").push(value.clone())));
        let runner = ExtensionRunner::new(loaded.extensions,loaded.runtime,loaded.events,context::create(&session,ui.clone()));
        session.set_extension_runner(runner).await;
        session.bind_extensions(ExtensionBindings {ui_context:Some(ui.clone() as Arc<dyn ExtensionUi>),mode:Some(ExtensionMode::Tui),..Default::default()}).await;
        session.bind_extensions(ExtensionBindings {ui_context:Some(ui.clone() as Arc<dyn ExtensionUi>),mode:Some(ExtensionMode::Tui),..Default::default()}).await;
        if late_rebind {
            let reopened = tokio::time::timeout(std::time::Duration::from_secs(5), opened_rx.recv()).await?.ok_or("Replacement UI did not open")?;
            if reopened != request || pending[0].owner.borrow().is_none() || completion.borrow().is_some() || !settlements.lock().expect("settlements").is_empty() {
                return Err("Late publication barrier did not preserve pending reattached request".into());
            }
            responses.as_ref().expect("response sender").send_replace(Some(QuestionResponse {status:QuestionStatus::Answered, answers:[(request.questions[0].id.clone(),QuestionAnswer {selected:vec!["A".into()],text:None})].into(),comment:None,unanswered:vec![],auto_resolved_after_ms:None}));
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop { if completion.borrow().is_some() { break; } completion.changed().await?; }
                Ok::<(), tokio::sync::watch::error::RecvError>(())
            }).await??;
        }
        Some(subscription)
    } else { None };
    let status = completion.borrow().as_ref().map(|response|response.status);
    let notification_count = settlements.lock().expect("settlements").len();
    {
        let events = settlements.lock().expect("settlements");
        if let Some(event) = events.first() {
            let replacement = replacement_context.lock().expect("replacement context");
            let expected_context = if reload { replacement.as_ref().ok_or("Replacement start context missing")? } else { &initial_owner.context };
            if event.ctx.session_manager.session_id() != session.session_id()
                || !Arc::ptr_eq(&event.ctx.ui, &expected_context.ui)
                || !Arc::ptr_eq(&event.ctx.model_registry, &expected_context.model_registry)
                || event.request != request
                || &event.response != completion.borrow().as_ref().ok_or("No response")? {
                return Err("Settled event lost owning context or complete request/response".into());
            }
            if !reload && (!Arc::ptr_eq(&event.ctx.session_manager, &initial_owner.context.session_manager) || !Arc::ptr_eq(&event.ctx.model_registry, &initial_owner.context.model_registry)) {
                return Err("Settled event changed owning context identity".into());
            }
            if reload && Arc::ptr_eq(&event.ctx.session_manager, &initial_owner.context.session_manager) {
                return Err("Reload settlement reused old context".into());
            }
        }
        if *legacy.lock().expect("legacy") != 0 { return Err("Duplicate legacy JSON notification".into()); }
    }
    if reload {
        let entries = session.with_session_manager(|manager| manager.entries());
        let persisted = entries.iter().filter(|entry| entry["type"] == "custom" && entry["customType"] == "ask-user:settlement" && entry["data"]["requestId"] == request.request_id).count();
        if persisted != 1 { return Err(format!("Reload settlement persistence: expected one, got {persisted}").into()); }
    }
    if fail_ui && !completion.borrow().as_ref().and_then(|response|response.comment.as_ref()).is_some_and(|comment|comment.contains("UI response channel closed")){return Err("Recovered UI failure lost error comment".into());}
    if status != Some(if fail_ui {QuestionStatus::OrphanedAfterRestart} else if timeout {QuestionStatus::TimedOut} else if cancel || abort {QuestionStatus::Cancelled} else {QuestionStatus::Answered}) || !get_pending_questions(&session.session_id()).is_empty() || notification_count != usize::from(!cancel && !abort) || request.request_id != pending[0].request.request_id {
        return Err(format!("Settlement receipt: status={status:?}, notifications={notification_count}").into());
    }
    if timeout && !reload {
        let asked_before=asked.lock().expect("asked").len();
        let retry=session.execute_tool(&tool,args,ExecuteToolOptions::default()).await?;
        if retry.details["status"]!="unavailable"||retry.details["accepted"]==true||!get_pending_questions(&session.session_id()).is_empty()||asked.lock().expect("asked").len()!=asked_before||opened_rx.try_recv().is_ok(){
            return Err("Timed-out turn opened a second question".into());
        }
    }
    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }.await;
    let mut cleanup_error = None;
    for entry in get_pending_questions(&session.session_id()) {
        (entry.cancel)(QuestionStatus::Cancelled);
        let mut completion = entry.completion.clone();
        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while completion.borrow().is_none() {
                if completion.changed().await.is_err() { break; }
            }
        }).await;
        if settled.is_err() { cleanup_error = Some("Question teardown did not settle"); }
        unregister_pending_question(&session.session_id(), &entry.request.request_id);
    }
    session.emit_session_shutdown(SessionReason::Quit).await;
    session.dispose().await;
    root.close()?;
    outcome?;
    if let Some(error) = cleanup_error { return Err(error.into()); }
    Ok(())
}

#[tokio::test]
async fn registered_async_question_delivers_one_settlement() { scenario(false, false, false, false, false).await.expect("registered answer"); }
#[tokio::test]
async fn registered_shutdown_settles_and_unregisters_before_returning() { scenario(true, false, false, false, false).await.expect("registered shutdown"); }

#[tokio::test]
async fn registered_abort_settles_and_unregisters_owned_question() { scenario(false, false, true, false, false).await.expect("registered abort"); }

#[tokio::test(start_paused = true)]
async fn registered_timeout_settles_once_and_unregisters_owned_question() { scenario(false, false, false, true, false).await.expect("registered timeout"); }

#[tokio::test(start_paused = true)]
async fn detached_timeout_queues_outcome_once_on_new_registered_runner() { scenario(false, false, false, true, true).await.expect("registered reload timeout"); }

#[tokio::test]
async fn detached_owner_rebinds_before_late_registered_publication() { scenario_order(false, false, false, false, true, true).await.expect("registered late rebind"); }

#[tokio::test]
async fn resumed_waiting_call_opens_original_request_without_duplicate_recovery() { scenario_recovery(false,false,false,false,false,false,true).await.expect("registered resume recovery"); }

#[tokio::test]
async fn recovered_ui_failure_settles_orphaned_with_comment_and_tears_down(){scenario_ui_failure(false,false,false,false,false,false,true,true).await.expect("recovered failure");}

struct FailingPersistence;
impl ExtensionActions for FailingPersistence {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { panic!("failed setup must not deliver") }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { panic!("failed setup must not inject") }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> { Err(ExtensionFailure::new("append failed")) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(Vec::new()) }
}
#[tokio::test]
async fn failed_persistence_leaves_no_pending_owner_or_notification() { scenario(false, true, false, false, false).await.expect("failed setup cleanup"); }
