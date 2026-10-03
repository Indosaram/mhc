#[path = "native_question/context.rs"]
mod context;
use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, ExecuteToolOptions};
use maho_ext_api::*;
use maho_ext_host::{ExtensionRunner, loader::{NativeExtensionFactory, load_extensions}};
use maho_ext_ask_user::{AskUser, registry::{get_pending_questions, unregister_pending_question}};
use serde_json::json;
use std::sync::{Arc, Mutex};

async fn scenario(cancel: bool, fail_append: bool, abort: bool, timeout: bool, reload: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)))), ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), true),
        cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None,
        excluded_tool_names: None, base_tools_override: None, session_start_event: None, auto_title_sessions: Some(false),
    })?;
    let loaded = load_extensions(vec![NativeExtensionFactory { path: "<ask-user-native>".into(), source_info: SourceInfo::default(), extension: Box::new(AskUser) }], project, ExtensionSessionProfile::default());
    let extension_runtime = loaded.runtime.clone();
    if !loaded.errors.is_empty() { return Err(format!("Factory errors: {:?}", loaded.errors).into()); }
    let (opened, mut opened_rx) = tokio::sync::mpsc::unbounded_channel();
    let (responses, response_rx) = tokio::sync::watch::channel(None);
    let ui = Arc::new(context::DecisionUi { opened, responses: response_rx });
    let mut event_context = context::create(&session, ui.clone());
    event_context.mode = ExtensionMode::Tui;
    let bus = loaded.events.clone();
    let settlements = Arc::new(Mutex::new(Vec::new()));
    let captured = settlements.clone();
    let _subscription = bus.on("ask-user:settled", Arc::new(move |value| captured.lock().expect("settlements").push(value.clone())));
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
        if !failed || pending != 0 || notifications != 0 || opened {
            return Err(format!("failed setup receipt: failed={failed}, pending={pending}, settlements={notifications}, opened={opened}, result={result:?}").into());
        }
        return Ok(());
    }
    let result = session.execute_tool(&tool, args, ExecuteToolOptions { signal: Some(signal.clone()), ..Default::default() }).await?;
    if result.details["accepted"] != true { return Err(format!("Question not accepted: {result:?}").into()); }
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), opened_rx.recv()).await?.ok_or("UI did not open")?;
    let pending = get_pending_questions(&session.session_id());
    if pending.len() != 1 { return Err(format!("Expected one pending question, got {}", pending.len()).into()); }
    let callback_owner = pending[0].owner.clone();
    let _reentrant_detach = bus.on("ask-user:settled",Arc::new(move |_| {
        callback_owner.send_replace(None);
    }));
    let mut completion = pending[0].completion.clone();
    if reload {
        responses.send_replace(Some(QuestionResponse {status:QuestionStatus::Answered,answers:[(request.questions[0].id.clone(),QuestionAnswer {selected:vec!["A".into()],text:None})].into(),comment:None,unanswered:vec![],auto_resolved_after_ms:None}));
        session.emit_session_shutdown(SessionReason::Reload).await;
        extension_runtime.invalidate("old question runner invalidated");
    }
    if timeout {
        tokio::time::advance(std::time::Duration::from_secs(30 * 60)).await;
    } else if abort {
        controller.abort(None);
    } else if cancel {
        session.emit_session_shutdown(SessionReason::Quit).await;
        if !get_pending_questions(&session.session_id()).is_empty() { return Err("Shutdown left pending question".into()); }
    } else {
        responses.send_replace(Some(QuestionResponse {status:QuestionStatus::Answered, answers:[(request.questions[0].id.clone(),QuestionAnswer {selected:vec!["A".into()],text:None})].into(),comment:None,unanswered:vec![],auto_resolved_after_ms:None}));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop { if completion.borrow().is_some() { break; } completion.changed().await?; }
        Ok::<(), tokio::sync::watch::error::RecvError>(())
    }).await??;
    let _new_subscription = if reload {
        if !settlements.lock().expect("settlements").is_empty() { return Err("Detached timeout notified old runner".into()); }
        let loaded = load_extensions(vec![NativeExtensionFactory { path:"<ask-user-reloaded>".into(),source_info:SourceInfo::default(),extension:Box::new(AskUser) }],project,ExtensionSessionProfile::default());
        if !loaded.errors.is_empty() { return Err(format!("Reload factory errors: {:?}",loaded.errors).into()); }
        let captured = settlements.clone();
        let subscription = loaded.events.on("ask-user:settled",Arc::new(move |value|captured.lock().expect("settlements").push(value.clone())));
        let runner = ExtensionRunner::new(loaded.extensions,loaded.runtime,loaded.events,context::create(&session,ui.clone()));
        session.set_extension_runner(runner).await;
        session.bind_extensions(ExtensionBindings {ui_context:Some(ui.clone() as Arc<dyn ExtensionUi>),mode:Some(ExtensionMode::Tui),..Default::default()}).await;
        session.bind_extensions(ExtensionBindings {ui_context:Some(ui.clone() as Arc<dyn ExtensionUi>),mode:Some(ExtensionMode::Tui),..Default::default()}).await;
        Some(subscription)
    } else { None };
    let status = completion.borrow().as_ref().map(|response|response.status);
    let notification_count = settlements.lock().expect("settlements").len();
    if reload {
        let entries = session.with_session_manager(|manager| manager.entries());
        let persisted = entries.iter().filter(|entry| entry["type"] == "custom" && entry["customType"] == "ask-user:settlement" && entry["data"]["requestId"] == request.request_id).count();
        if persisted != 1 { return Err(format!("Reload settlement persistence: expected one, got {persisted}").into()); }
    }
    if status != Some(if timeout {QuestionStatus::TimedOut} else if cancel || abort {QuestionStatus::Cancelled} else {QuestionStatus::Answered}) || !get_pending_questions(&session.session_id()).is_empty() || notification_count != usize::from(!cancel && !abort) || request.request_id != pending[0].request.request_id {
        return Err(format!("Settlement receipt: status={status:?}, notifications={notification_count}").into());
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

struct FailingPersistence;
impl ExtensionActions for FailingPersistence {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> { panic!("failed setup must not deliver") }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { panic!("failed setup must not inject") }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> { Err(ExtensionFailure::new("append failed")) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(Vec::new()) }
}
#[tokio::test]
async fn failed_persistence_leaves_no_pending_owner_or_notification() { scenario(false, true, false, false, false).await.expect("failed setup cleanup"); }
