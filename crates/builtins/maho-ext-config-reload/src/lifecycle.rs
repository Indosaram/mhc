use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use maho_ext_api::{EventBus, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionMode, NotificationType};
use crate::{index::*, log::{ConfigReloadLogger, LogEvent, LogLevel}, protocol::*, watch_engine::NativeWatchEngine};

#[derive(Default)]
pub struct ConfigReload;
struct ReloadHandoff { hashes: BTreeMap<PathBuf, String>, contents: BTreeMap<PathBuf, String>, requested: std::time::Instant, changes: Vec<PendingChange> }
static HANDOFFS: std::sync::OnceLock<Mutex<ConfigReloadHandoffRegistry<ReloadHandoff>>> = std::sync::OnceLock::new();
struct WatchRun { cancel: tokio::sync::watch::Sender<bool>, task: tokio::task::JoinHandle<Result<(), String>> }
#[derive(Default)]
struct State { generation: u64, run: Option<WatchRun>, pending: PendingChanges, in_flight: bool, deferred_notice: bool, unavailable_logged: bool, veto: crate::reload_deferral::ReloadVetoDeferral, flush_generation: Option<u64>, hashes: BTreeMap<PathBuf, String>, contents: BTreeMap<PathBuf, String>, registrations: WatchRegistrations, context: Option<ExtensionContext>, rebuild: Arc<tokio::sync::Notify>, subscriptions: Vec<maho_ext_api::BusSubscription>, session_owned: bool }
impl Extension for ConfigReload {
    fn register(&self, api: &mut ExtensionApi) {
        let state = Arc::new(Mutex::new(State { session_owned: maho_ai::node::provider_scope::active_provider_scope().is_some(), ..Default::default() }));
        let owner = Arc::downgrade(&state);
        let events = api.events.clone();
        let cwd = api.cwd.clone();
        let subscription = api.on_config_watch_registration(Arc::new(move |_, registration| {
            if let Some(state) = owner.upgrade() { admit_native_registration(&state, registration, &cwd, &events); }
        }));
        state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).subscriptions.push(subscription);
        for (_, registration) in api.events.config_watch_registrations() { admit_native_registration(&state, &registration, &api.cwd, &api.events); }
        for channel in [CONFIG_WATCH_REGISTER, CONFIG_WATCH_UNREGISTER] {
            let owner = Arc::downgrade(&state);
            let events = api.events.clone();
            let cwd = api.cwd.clone();
            let subscription = api.events.on(channel, Arc::new(move |payload| {
                if channel == CONFIG_WATCH_REGISTER && events.config_watch_registrations().iter().any(|(_, registration)| payload.get("id").and_then(serde_json::Value::as_str) == Some(registration.id.as_str())) { return; }
                let Some(state) = owner.upgrade() else { return; };
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let (cwd, agent_dir) = state.context.as_ref().map_or_else(|| (cwd.clone(), PathBuf::from(maho_core::config::get_agent_dir())), |ctx| (ctx.cwd.clone(), ctx.agent_dir.clone()));
                let State { registrations, pending, .. } = &mut *state;
                let mut rejection = None;
                let mut logger = ConfigReloadLogger::new(&agent_dir, None).ok();
                let changed = if channel == CONFIG_WATCH_REGISTER {
                    let Some(registration) = parse_config_watch_registration(payload) else { return; };
                    let id = registration.id.clone();
                    match registrations.register(Arc::new(registration), &cwd, &agent_dir, pending) {
                        RegistrationAdmission::Added => { if let Some(logger) = &mut logger { logger.log(LogLevel::Info, LogEvent::RegistrationAdded { id: &id }); } true },
                        RegistrationAdmission::Restricted => { if let Some(logger) = &mut logger { logger.log(LogLevel::Warn, LogEvent::RegistrationRejected { registration_id: &id, error_count: 1.0 }); } rejection = Some(id); false },
                        RegistrationAdmission::Identical => false,
                        RegistrationAdmission::RejectionSuppressed => { if let Some(logger) = &mut logger { logger.log(LogLevel::Debug, LogEvent::RegistrationRejectionSuppressed { registration_id: &id }); } false },
                    }
                } else {
                    let Some(id) = payload.get("id").and_then(serde_json::Value::as_str) else { return; };
                    let removed = registrations.unregister(id, pending);
                    if removed && let Some(logger) = &mut logger { logger.log(LogLevel::Info, LogEvent::RegistrationRemoved { id }); }
                    removed
                };
                if changed && state.run.is_some() { state.rebuild.notify_one(); }
                drop(state);
                if let Some(id) = rejection { events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({"registrationId":id,"paths":[],"errors":["Configuration watch target is restricted"]})); }
            }));
            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).subscriptions.push(subscription);
        }
        let shared = Arc::clone(&state);
        let events = api.events.clone();
        api.on(EventKind::SessionStart, Arc::new(move |event, ctx| {
            let state = Arc::clone(&shared); let events = events.clone();
            Box::pin(async move {
                let reloading = matches!(event, maho_ext_api::ExtensionEvent::SessionStart(event) if event.reason == maho_ext_api::SessionReason::Reload);
                start(Arc::clone(&state), ctx.clone(), events.clone(), true).await?;
                if reloading {
                    let handoff = HANDOFFS.get_or_init(Mutex::default).lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.take(&handoff_key(&state, ctx)?);
                    if let Some(handoff) = handoff {
                        let mut paths = std::collections::BTreeSet::new();
                        for change in &handoff.changes {
                            paths.extend(change.paths.iter().cloned());
                            events.emit(CONFIG_WATCH_RELOADED, &serde_json::json!({"registrationId":change.registration_id,"paths":change.paths}));
                        }
                        ctx.ui.notify(&format!("Hot-reloaded: {}", paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join(", ")), NotificationType::Info);
                        let mut logger = ConfigReloadLogger::new(&ctx.agent_dir, None).map_err(|error| ExtensionFailure::new(error.to_string()))?;
                        logger.log(LogLevel::Info, LogEvent::ReloadCompleted { duration_ms: handoff.requested.elapsed().as_secs_f64() * 1000.0 });
                        let (changed, rejected) = {
                            let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
                            let changed = compare_snapshots(&handoff.hashes, &state.hashes);
                            let snapshot = state.hashes.clone();
                            let mut rejected = None;
                            if !changed.is_empty() {
                                state.contents = handoff.contents;
                                let significant = significant_changed_paths(&changed, &snapshot, &mut state.contents, &ctx.agent_dir, &ctx.cwd, &mut logger);
                                let errors = validate_builtin_paths(&significant, &ctx.agent_dir, &ctx.cwd);
                                if errors.is_empty() && !significant.is_empty() { state.pending.add("builtin", &significant); }
                                else if !errors.is_empty() { rejected = Some((significant, errors)); }
                            }
                            (changed, rejected)
                        };
                        if let Some((paths, errors)) = rejected {
                            ctx.ui.notify(&format!("Config change rejected: {}", errors.join("; ")), NotificationType::Error);
                            events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({"registrationId":"builtin","paths":paths,"errors":errors}));
                        }
                        if !changed.is_empty() { schedule_flush(state, ctx.clone(), None)?; }
                    }
                }
                Ok(EventResult::None)
            })
        }));
        for kind in [EventKind::AgentEnd, EventKind::AgentSettled] {
            let state = Arc::clone(&state);
            api.on(kind, Arc::new(move |_, ctx| {
                let state = Arc::clone(&state);
                Box::pin(async move { schedule_flush(state, ctx.clone(), None)?; Ok(EventResult::None) })
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
        api.on(EventKind::SessionShutdown, Arc::new(move |event, ctx| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                if !matches!(event, maho_ext_api::ExtensionEvent::SessionShutdown(event) if event.reason == maho_ext_api::SessionReason::Reload) {
                    HANDOFFS.get_or_init(Mutex::default).lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.delete(&handoff_key(&state, ctx)?);
                }
                stop(&state, true).await?;
                let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
                state.context = None;
                state.subscriptions.clear();
                Ok(EventResult::None)
            })
        }));
    }
}
async fn stop(state: &Arc<Mutex<State>>, clear_pending: bool) -> Result<(), ExtensionFailure> {
    let run = {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        state.generation = state.generation.wrapping_add(1);
        state.flush_generation = None;
        state.veto.reset();
        if clear_pending { state.pending.clear(); state.in_flight = false; state.deferred_notice = false; state.unavailable_logged = false; }
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
    state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.context = Some(ctx.clone());
    let home = std::env::var("HOME").unwrap_or_default();
    let settings = maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(), &ctx.agent_dir.to_string_lossy(), &home, ctx.is_project_trusted());
    let resolved = resolve_config_reload_settings(&serde_json::json!(settings.get_global()), &serde_json::json!(settings.get_project()));
    if !resolved.enabled || matches!(ctx.mode, ExtensionMode::Print | ExtensionMode::Json) || ctx.mode == ExtensionMode::Rpc && ctx.session_manager.session_file().is_none() {
        events.emit(CONFIG_WATCH_READY, &serde_json::json!({"enabled":false})); return Ok(());
    }
    let skill_paths: Vec<PathBuf> = settings.get_value("skills").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(serde_json::Value::as_str).map(PathBuf::from).collect();
    let mut targets = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
    let external = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.registrations.snapshot();
    targets.extend(build_external_watch_targets(&ctx.cwd, &external));
    let mut watched_targets = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
    watched_targets.extend(build_external_watch_targets(&ctx.cwd, &external));
    let watched = watched_targets.into_iter().map(|active| active.target).collect();
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
    {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        state.hashes = engine.engine.get_baseline_snapshot(); state.contents = contents.clone();
    }
    logger.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.log(LogLevel::Info, LogEvent::WatcherStarted { target_count: targets.len() as f64 });
    let generation = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.generation;
    let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
    let shared = Arc::clone(&state);
    let rebuild = Arc::clone(&state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.rebuild);
    let ready_events = events.clone();
    let scope = maho_ai::node::provider_scope::active_provider_scope();
    let work = async move {
        loop {
            let change = tokio::select! {
                result = engine.next_change_async() => result?,
                () = rebuild.notified() => {
                    engine.close_async().await?;
                    let registrations = shared.lock().map_err(|error| error.to_string())?.registrations.snapshot();
                    targets = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
                    targets.extend(build_external_watch_targets(&ctx.cwd, &registrations));
                    let mut watched = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
                    watched.extend(build_external_watch_targets(&ctx.cwd, &registrations));
                    engine = NativeWatchEngine::with_debounce(watched.into_iter().map(|active| active.target).collect(), Arc::clone(&on_error), debounce)?;
                    events.emit(CONFIG_WATCH_READY, &serde_json::json!({"enabled":true}));
                    continue;
                },
                result = cancelled.changed() => { if result.is_err() || *cancelled.borrow() { break; } else { continue; } },
            };
            let paths = {
                let mut logger = logger.lock().map_err(|error| error.to_string())?;
                significant_changed_paths(&change.changed_paths, &engine.engine.get_baseline_snapshot(), &mut contents, &ctx.agent_dir, &ctx.cwd, &mut logger)
            };
            {
                let mut state = shared.lock().map_err(|error| error.to_string())?;
                state.hashes = engine.engine.get_baseline_snapshot(); state.contents = contents.clone();
            }
            for (id, paths) in group_changed_paths(&paths, &targets) {
                let errors = if id == "builtin" { validate_builtin_paths(&paths, &ctx.agent_dir, &ctx.cwd) } else {
                    let validator = shared.lock().map_err(|error| error.to_string())?.registrations.validator(&id);
                    validate_external_paths(validator, &paths)
                };
                if !errors.is_empty() {
                    ctx.ui.notify(&format!("Config change rejected: {}", errors.join("; ")), NotificationType::Error);
                    logger.lock().map_err(|error| error.to_string())?.log(LogLevel::Warn, LogEvent::ValidationRejected { registration_id: &id, error_count: errors.len() as f64 });
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
                let logged_paths = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>();
                logger.lock().map_err(|error| error.to_string())?.log(LogLevel::Info, LogEvent::ChangeDetected { registration_id: &id, paths: &logged_paths, deferred });
                events.emit(CONFIG_WATCH_CHANGED, &serde_json::json!({"registrationId":id,"paths":paths,"deferred":deferred}));
            }
            if change.created.iter().any(|path| targets.iter().any(|target| target.rearm_on_creation.as_ref() == Some(path))) {
                engine.close_async().await?;
                targets = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
                let registrations = shared.lock().map_err(|error| error.to_string())?.registrations.snapshot();
                targets.extend(build_external_watch_targets(&ctx.cwd, &registrations));
                let mut watched = build_builtin_watch_targets(&ctx.cwd, &ctx.agent_dir, ctx.is_project_trusted(), &resolved, &skill_paths);
                watched.extend(build_external_watch_targets(&ctx.cwd, &registrations));
                let watched = watched.into_iter().map(|active| active.target).collect();
                engine = NativeWatchEngine::with_debounce(watched, Arc::clone(&on_error), debounce)?;
                crate::routine_settings::refresh_settings_content_snapshots(&mut contents, &ctx.agent_dir, &ctx.cwd);
                logger.lock().map_err(|error| error.to_string())?.log(LogLevel::Info, LogEvent::WatcherStarted { target_count: targets.len() as f64 });
                events.emit(CONFIG_WATCH_READY, &serde_json::json!({"enabled":true}));
            }
            schedule_flush(Arc::clone(&shared), ctx.clone(), Some(generation)).map_err(|error| error.to_string())?;
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
fn schedule_flush(state: Arc<Mutex<State>>, ctx: ExtensionContext, expected_generation: Option<u64>) -> Result<(), ExtensionFailure> {
    let (generation, mut cancelled) = {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if expected_generation.is_some_and(|generation| generation != state.generation) { return Ok(()); }
        let Some(run) = &state.run else { return Ok(()); };
        let cancelled = run.cancel.subscribe();
        let generation = state.generation;
        if state.flush_generation == Some(generation) { return Ok(()); }
        state.flush_generation = Some(generation);
        (generation, cancelled)
    };
    let scope = maho_ai::node::provider_scope::active_provider_scope();
    tokio::spawn(async move {
        let work = async {
            loop {
                if *cancelled.borrow() { break; }
                let delay = match flush(Arc::clone(&state), ctx.clone(), Some(generation)).await {
                    Ok(Some(delay)) => delay,
                    Ok(None) => break,
                    Err(error) => { ctx.ui.notify(&error.to_string(), NotificationType::Error); break; },
                };
                tokio::select! {
                    () = tokio::time::sleep(delay) => {},
                    _ = cancelled.changed() => break,
                }
            }
        };
        if let Some(scope) = scope { let _ = maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, work).await; } else { work.await; }
        if let Ok(mut state) = state.lock() && state.flush_generation == Some(generation) { state.flush_generation = None; }
    });
    Ok(())
}
async fn flush(state: Arc<Mutex<State>>, ctx: ExtensionContext, expected_generation: Option<u64>) -> Result<Option<Duration>, ExtensionFailure> {
    let generation = {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if expected_generation.is_some_and(|generation| generation != state.generation) { return Ok(None); }
        match reload_admission(state.pending.is_empty(), state.in_flight, ctx.is_idle(), ctx.has_pending_messages()?, ctx.is_compacting(), ctx.actions().is_ok()) {
            ReloadAdmission::ProbeVeto => {},
            ReloadAdmission::Compacting => return Ok(Some(Duration::from_millis(250))),
            ReloadAdmission::Unavailable => {
                if !state.unavailable_logged {
                    state.unavailable_logged = true;
                    let mut logger = ConfigReloadLogger::new(&ctx.agent_dir, None).map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    logger.log(LogLevel::Info, LogEvent::ReloadRequested { reason: "requestReload unavailable", paths: &[] });
                }
                return Ok(None);
            },
            _ => return Ok(None),
        }
        state.generation
    };
    let veto = ctx.check_reload_veto().await?;
    if veto.cancelled {
        let notice = {
            let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
            if state.generation != generation || state.in_flight || state.pending.is_empty() { return Ok(None); }
            state.veto.defer(veto.reason.as_deref())
        };
        if let Some(notice) = notice { ctx.ui.notify(&notice, NotificationType::Info); }
        let mut logger = ConfigReloadLogger::new(&ctx.agent_dir, None).map_err(|error| ExtensionFailure::new(error.to_string()))?;
        logger.log(LogLevel::Info, LogEvent::ReloadDeferred { reason: veto.reason.as_deref().unwrap_or("extension veto") });
        return Ok(Some(Duration::from_millis(1000)));
    }
    let (paths, handoff) = {
        let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
        if state.generation != generation || state.in_flight || state.pending.is_empty() { return Ok(None); }
        state.veto.reset();
        state.in_flight = true;
        let changes = state.pending.snapshot();
        let paths = changes.iter().flat_map(|change| change.paths.iter().cloned()).collect::<std::collections::BTreeSet<_>>();
        let handoff = ReloadHandoff { hashes: state.hashes.clone(), contents: state.contents.clone(), requested: std::time::Instant::now(), changes };
        (paths, handoff)
    };
    HANDOFFS.get_or_init(Mutex::default).lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.set(handoff_key(&state, &ctx)?, handoff);
    let mut logger = ConfigReloadLogger::new(&ctx.agent_dir, None).map_err(|error| ExtensionFailure::new(error.to_string()))?;
    let logged_paths = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>();
    logger.log(LogLevel::Info, LogEvent::ReloadRequested { reason: "config changed", paths: &logged_paths });
    ctx.ui.notify(&format!("Hot-reloading: {}", paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join(", ")), NotificationType::Info);
    let result = ctx.request_reload().await;
    HANDOFFS.get_or_init(Mutex::default).lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.delete(&handoff_key(&state, &ctx)?);
    let mut state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
    if state.generation == generation { state.in_flight = false; }
    if let Err(error) = result { logger.log(LogLevel::Error, LogEvent::WatcherError { path: "reload", message: &error.to_string() }); }
    Ok(None)
}
fn admit_native_registration(state: &Arc<Mutex<State>>, registration: &maho_ext_api::RegisteredConfigWatch, cwd: &std::path::Path, events: &EventBus) {
    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (cwd, agent_dir) = state.context.as_ref().map_or_else(|| (cwd.to_path_buf(), PathBuf::from(maho_core::config::get_agent_dir())), |ctx| (ctx.cwd.clone(), ctx.agent_dir.clone()));
    let State { registrations, pending, .. } = &mut *state;
    let admission = registrations.register_native(registration, &cwd, &agent_dir, pending);
    if admission == RegistrationAdmission::Added && state.run.is_some() { state.rebuild.notify_one(); }
    let mut logger = ConfigReloadLogger::new(&agent_dir, None).ok();
    if let Some(logger) = &mut logger {
        match admission {
            RegistrationAdmission::Added => { logger.log(LogLevel::Info, LogEvent::RegistrationAdded { id: &registration.id }); },
            RegistrationAdmission::Restricted => { logger.log(LogLevel::Warn, LogEvent::RegistrationRejected { registration_id: &registration.id, error_count: 1.0 }); },
            RegistrationAdmission::RejectionSuppressed => { logger.log(LogLevel::Debug, LogEvent::RegistrationRejectionSuppressed { registration_id: &registration.id }); },
            RegistrationAdmission::Identical => {},
        }
    }
    drop(state);
    if admission == RegistrationAdmission::Restricted { events.emit(CONFIG_WATCH_REJECTED, &serde_json::json!({"registrationId":registration.id,"paths":[],"errors":["Configuration watch target is restricted"]})); }
}

fn handoff_key(state: &Arc<Mutex<State>>, ctx: &ExtensionContext) -> Result<String, ExtensionFailure> {
    let state = state.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?;
    Ok(if state.session_owned { ctx.session_manager.session_id().into() } else { "classic".into() })
}
