use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use maho_ext_api::{EventBus, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionMode, NotificationType};
use crate::{index::*, log::{ConfigReloadLogger, LogEvent, LogLevel}, protocol::*, watch_engine::NativeWatchEngine};

#[derive(Default)]
pub struct ConfigReload;
struct WatchRun { cancel: tokio::sync::watch::Sender<bool>, task: tokio::task::JoinHandle<Result<(), String>> }
#[derive(Default)]
struct State { generation: u64, run: Option<WatchRun>, pending: PendingChanges, in_flight: bool, deferred_notice: bool }
impl Extension for ConfigReload {
    fn register(&self, api: &mut ExtensionApi) {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let events = api.events.clone();
        api.on(EventKind::SessionStart, Arc::new(move |_, ctx| {
            let state = Arc::clone(&shared); let events = events.clone();
            Box::pin(async move { start(state, ctx.clone(), events, true).await?; Ok(EventResult::None) })
        }));
        for kind in [EventKind::AgentEnd, EventKind::AgentSettled] {
            let state = Arc::clone(&state);
            api.on(kind, Arc::new(move |_, ctx| {
                let state = Arc::clone(&state);
                Box::pin(async move { flush(state, ctx.clone(), None).await?; Ok(EventResult::None) })
            }));
        }
        let shared = Arc::clone(&state);
        let events = api.events.clone();
        api.on(EventKind::ProjectTrust, Arc::new(move |_, ctx| {
            let state = Arc::clone(&shared); let events = events.clone();
            Box::pin(async move {
                start(state, ctx.clone(), events, false).await?;
                Ok(EventResult::ProjectTrust(maho_ext_api::ProjectTrustEventResult { trusted: maho_ext_api::TrustDecision::Undecided, remember: None }))
            })
        }));
        api.on(EventKind::SessionShutdown, Arc::new(move |_, _| {
            let state = Arc::clone(&state);
            Box::pin(async move { stop(&state, true).await?; Ok(EventResult::None) })
        }));
    }
}
async fn stop(state: &Arc<Mutex<State>>, clear_pending: bool) -> Result<(), ExtensionFailure> {
    let run = {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        state.generation = state.generation.wrapping_add(1);
        if clear_pending { state.pending.clear(); state.in_flight = false; state.deferred_notice = false; }
        state.run.take()
    };
    if let Some(run) = run {
        run.cancel.send_replace(true);
        run.task.await.map_err(|error| ExtensionFailure::new(error.to_string()))?.map_err(ExtensionFailure::new)?;
    }
    Ok(())
}
async fn start(state: Arc<Mutex<State>>, ctx: ExtensionContext, events: EventBus, clear_pending: bool) -> Result<(), ExtensionFailure> {
    stop(&state, clear_pending).await?;
    let home = std::env::var("HOME").unwrap_or_default();
    let settings = maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(), &ctx.agent_dir.to_string_lossy(), &home, ctx.is_project_trusted());
    let resolved = resolve_config_reload_settings(&serde_json::json!(settings.get_global()), &serde_json::json!(settings.get_project()));
    if !resolved.enabled || matches!(ctx.mode, ExtensionMode::Print | ExtensionMode::Json) || ctx.mode == ExtensionMode::Rpc && ctx.session_manager.session_file().is_none() {
        events.emit(CONFIG_WATCH_READY, &serde_json::json!({"enabled":false})); return Ok(());
    }
    let skill_paths: Vec<PathBuf> = settings.get_value("skills").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(serde_json::Value::as_str).map(PathBuf::from).collect();
    let mut targets = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
    let watched = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths).into_iter().map(|active| active.target).collect();
    let logger = Arc::new(Mutex::new(ConfigReloadLogger::new(&ctx.agent_dir, None).map_err(|error| ExtensionFailure::new(error.to_string()))?));
    let errors = Arc::clone(&logger);
    let scoped_errors = crate::session_scoped_callback::bind_session_scoped_callback(move |(message, path): (String, PathBuf)| {
        if let Ok(mut logger) = errors.lock() { logger.log(LogLevel::Error, LogEvent::WatcherError { path: &path.to_string_lossy(), message: &message }); }
    });
    let on_error: crate::watch_event_source::WatchErrorListener = Arc::new(move |message, path| {
        let _ = scoped_errors((message, path));
    });
    let debounce = Duration::from_secs_f64(resolved.debounce_ms / 1000.0);
    let mut engine = NativeWatchEngine::with_debounce(watched, Arc::clone(&on_error), debounce).map_err(ExtensionFailure::new)?;
    let mut contents = BTreeMap::new();
    crate::routine_settings::refresh_settings_content_snapshots(&mut contents, &ctx.agent_dir, &ctx.cwd);
    let generation = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.generation;
    let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
    let shared = Arc::clone(&state);
    let ready_events = events.clone();
    let scope = maho_ai::node::provider_scope::active_provider_scope();
    let work = async move {
        loop {
            let change = tokio::select! {
                result = engine.next_change_async() => result?,
                result = cancelled.changed() => { if result.is_err() || *cancelled.borrow() { break; } else { continue; } },
            };
            let paths = {
                let mut logger = logger.lock().map_err(|error| error.to_string())?;
                significant_changed_paths(&change.changed_paths, &engine.engine.get_baseline_snapshot(), &mut contents, &ctx.agent_dir, &ctx.cwd, &mut logger)
            };
            for (id, paths) in group_changed_paths(&paths, &targets) {
                let errors = validate_builtin_paths(&paths, &ctx.agent_dir, &ctx.cwd);
                if !errors.is_empty() {
                    ctx.ui.notify(&format!("Config change rejected: {}", errors.join("; ")), NotificationType::Error);
                    events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({"registrationId":id,"paths":paths,"errors":errors}));
                    continue;
                }
                let deferred = !ctx.is_idle() || ctx.has_pending_messages().map_err(|error| error.to_string())? || ctx.is_compacting();
                {
                    let mut state = shared.lock().map_err(|error| error.to_string())?;
                    if state.generation != generation || state.in_flight { continue; }
                    state.pending.add(&id, &paths);
                    if deferred && !state.deferred_notice { state.deferred_notice = true; ctx.ui.notify("Config changed; reloading when idle", NotificationType::Info); }
                }
                events.emit(CONFIG_WATCH_CHANGED, &serde_json::json!({"registrationId":id,"paths":paths,"deferred":deferred}));
            }
            if change.created.iter().any(|path| targets.iter().any(|target| target.rearm_on_creation.as_ref() == Some(path))) {
                engine.close_async().await?;
                targets = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
                let watched = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths).into_iter().map(|active| active.target).collect();
                engine = NativeWatchEngine::with_debounce(watched, Arc::clone(&on_error), debounce)?;
                crate::routine_settings::refresh_settings_content_snapshots(&mut contents, &ctx.agent_dir, &ctx.cwd);
                events.emit(CONFIG_WATCH_READY, &serde_json::json!({"enabled":true}));
            }
            let state = Arc::clone(&shared); let context = ctx.clone();
            let scope = maho_ai::node::provider_scope::active_provider_scope();
            tokio::spawn(async move {
                let work = async { if let Err(error) = flush(state, context.clone(), Some(generation)).await { context.ui.notify(&error.to_string(), NotificationType::Error); } };
                if let Some(scope) = scope { let _ = maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, work).await; } else { work.await; }
            });
        }
        engine.close_async().await
    };
    let task = tokio::spawn(async move {
        if let Some(scope) = scope { maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, work).await.map_err(|error| error.to_string())? } else { work.await }
    });
    state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.run = Some(WatchRun { cancel, task });
    ready_events.emit(CONFIG_WATCH_READY, &serde_json::json!({"enabled":true}));
    Ok(())
}
async fn flush(state: Arc<Mutex<State>>, ctx: ExtensionContext, expected_generation: Option<u64>) -> Result<(), ExtensionFailure> {
    let generation = {
        let state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if expected_generation.is_some_and(|generation| generation != state.generation) { return Ok(()); }
        if reload_admission(state.pending.is_empty(), state.in_flight, ctx.is_idle(), ctx.has_pending_messages()?, ctx.is_compacting(), ctx.actions().is_ok()) != ReloadAdmission::ProbeVeto { return Ok(()); }
        state.generation
    };
    let veto = ctx.check_reload_veto().await?;
    if veto.cancelled { return Ok(()); }
    let paths = {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if state.generation != generation || state.in_flight || state.pending.is_empty() { return Ok(()); }
        state.in_flight = true;
        state.pending.snapshot().into_iter().flat_map(|change| change.paths).collect::<std::collections::BTreeSet<_>>()
    };
    ctx.ui.notify(&format!("Hot-reloading: {}", paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join(", ")), NotificationType::Info);
    let result = ctx.request_reload().await;
    let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
    if state.generation == generation { state.in_flight = false; }
    result
}
