use crate::{
    status_live::MemoryFooterUi,
    wiring_reflection_live::MemoryReflectionLiveWiring,
    wiring_runtime::MemoryRuntimeWiring,
    wiring_types::{MemoryAfterBindInput, MemoryWiringOptions},
};
pub struct MemoryWiring {
    pub runtime: MemoryRuntimeWiring,
    pub reflection_live: MemoryReflectionLiveWiring,
    pub skills_usage: crate::skills_usage_wiring::SkillsUsageTrackers,
    pub maintenance: crate::memory_maintenance::MemoryMaintenance,
    evaluators: Vec<Box<dyn crate::shutdown_drain::ShutdownEvaluator>>,
    pub seams: crate::wiring_types::MemoryWiringSeams,
    live_host: Option<(
        std::sync::Arc<maho_ext_api::ExtensionApi>,
        std::sync::Arc<dyn maho_ext_api::ExtensionUi>,
    )>,
    footer_ticker: bool,
    pub provenance_by_session: std::collections::HashMap<String, maho_ext_api::ForkProvenance>,
    /// Which sessions this run accepted input from and which finished their initial reconciliation.
    pub user_activity: crate::user_activity::MemoryUserActivity,
    /// Stuck-holder notices already shown, keyed `<session>:<run>`: a holder that stays held is reported once.
    stale_lock_notices_shown: std::collections::BTreeSet<String>,
    /// Holders this session already asked reconciliation to settle, keyed `<session>:<run>`.
    stale_holder_reconciles: std::collections::BTreeSet<String>,
}
pub fn create_memory_wiring(options: MemoryWiringOptions) -> MemoryWiring {
    MemoryWiring {
        runtime: options.runtime,
        reflection_live: Default::default(),
        skills_usage: options.skills_usage,
        maintenance: crate::memory_maintenance::MemoryMaintenance::new(Default::default()),
        evaluators: vec![],
        seams: Default::default(),
        live_host: None,
        footer_ticker: false,
        provenance_by_session: Default::default(),
        user_activity: Default::default(),
        stale_lock_notices_shown: Default::default(),
        stale_holder_reconciles: Default::default(),
    }
}
/// The host UI the sync notices are written to (`SyncNoticeUi`).
struct NativeSyncNotice(std::sync::Arc<dyn maho_ext_api::ExtensionUi>);
impl crate::sync_status_cached::SyncNoticeUi for NativeSyncNotice {
    fn notify(&self, message: &str, level: &str) {
        let kind = match level {
            "error" => maho_ext_api::NotificationType::Error,
            "warning" => maho_ext_api::NotificationType::Warning,
            _ => maho_ext_api::NotificationType::Info,
        };
        self.0.notify(message, kind);
    }
}
struct NativeFooter(std::sync::Arc<dyn maho_ext_api::ExtensionUi>);
impl MemoryFooterUi for NativeFooter {
    fn set_status(&mut self, key: &str, text: &str) {
        self.0.set_status(key, Some(text));
    }
}
/// Bound-session live surface for the bind-time completion drain (pin `wiring-reflection-live.ts`
/// bind: liveSession built from createReflectionCompletionApi(pi) and readUi(eventCtx)).
struct NativeBindLive<'a> {
    api: maho_ext_api::ExtensionApi,
    context: &'a maho_ext_api::ExtensionContext,
    initial: crate::wiring_types::NativeInitialStatus,
    warn: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
}
impl crate::worker::completion_delivery::ReflectionLiveSession for NativeBindLive<'_> {
    fn session_id(&self) -> &str {
        self.context.session_manager.session_id()
    }
    fn append_entry(&mut self, kind: &str, data: serde_json::Value) {
        if let Err(error) = self.api.append_entry(kind, Some(data)) {
            // A retired ctx means the session this entry would journal into is gone (#7946, #10339): best-effort, not a failure.
            if !crate::stale_extension_context::is_stale_extension_context_error(&error.to_string())
            {
                (self.warn)(&format!("memory reflection entry failed: {error}"));
            }
        }
    }
    fn notify(&mut self, message: &str, warning: bool) -> Result<(), String> {
        self.context.ui.notify(
            message,
            if warning {
                maho_ext_api::NotificationType::Warning
            } else {
                maho_ext_api::NotificationType::Info
            },
        );
        Ok(())
    }
    fn warn(&mut self, message: &str, error: &str) {
        (self.warn)(&format!("{message}: {error}"));
    }
    fn on_completion(&mut self, _: &str) {}
}
impl crate::worker::health_alert::ReflectionHealthLiveSession for NativeBindLive<'_> {
    fn session_id(&self) -> &str {
        self.context.session_manager.session_id()
    }
    fn has_ui(&self) -> bool {
        self.context.has_ui
    }
    fn append_entry(
        &mut self,
        name: &str,
        entry: &crate::worker::health_alert::ReflectionHealthEntry,
    ) {
        match serde_json::to_value(entry) {
            Ok(data) => crate::worker::completion_delivery::ReflectionLiveSession::append_entry(
                self, name, data,
            ),
            Err(error) => (self.warn)(&error.to_string()),
        }
    }
    fn notify(&mut self, message: &str, _: &str) {
        self.context
            .ui
            .notify(message, maho_ext_api::NotificationType::Warning);
    }
}
impl crate::wiring_reflection_live::ReflectionBindLive for NativeBindLive<'_> {
    fn refresh_initial_status(&mut self, identity: &crate::context::MemoryIdentityContext) {
        (self.initial)(identity, self.context);
    }
    fn completions(
        &mut self,
    ) -> &mut dyn crate::worker::completion_delivery::ReflectionLiveSession {
        self
    }
    fn health(&mut self) -> &mut dyn crate::worker::health_alert::ReflectionHealthLiveSession {
        self
    }
}
/// 320ms footer animation driver (pin `status-live.ts` timers.set(tick, FRAME_INTERVAL_MS)).
/// Runs on its own thread and exits once the footer stops animating; call outside async context.
pub fn spawn_footer_ticker(
    wiring: std::sync::Arc<tokio::sync::Mutex<MemoryWiring>>,
) -> std::io::Result<()> {
    let ticker = wiring.clone();
    let spawned = std::thread::Builder::new()
        .name("memory-footer".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(
                    crate::status_live::FRAME_INTERVAL_MS,
                ));
                let mut guard = ticker.blocking_lock();
                let ui = guard.live_host.as_ref().map(|(_, ui)| ui.clone());
                match ui {
                    Some(ui) if guard.reflection_live.footer.is_animating() => {
                        guard.reflection_live.footer.tick(&mut NativeFooter(ui))
                    }
                    _ => {
                        guard.footer_ticker = false;
                        return;
                    }
                }
            }
        });
    if spawned.is_err()
        && let Ok(mut guard) = wiring.try_lock()
    {
        guard.footer_ticker = false;
    }
    spawned.map(|_| ())
}
/// Pin `memory/index.ts:100-111` + `wiring-runtime.ts:51-52`: `session_start` replaces the session
/// state, so a session that does not rebind (disabled, identity conflict) resolves no context.
/// Registered before the component's own `session_start`, which rebinds through `after_bind`.
fn register_session_reset_handler(
    api: &mut maho_ext_api::ExtensionApi,
    wiring: std::sync::Arc<tokio::sync::Mutex<MemoryWiring>>,
) {
    api.on(
        maho_ext_api::EventKind::SessionStart,
        std::sync::Arc::new(move |_, context| {
            let wiring = wiring.clone();
            Box::pin(async move {
                wiring
                    .lock()
                    .await
                    .unbind(context.session_manager.session_id());
                Ok(maho_ext_api::EventResult::None)
            })
        }),
    );
}
/// Production settled handler (pin `wiring-static.ts:96-111` agent_settled: journal reconcile, facts, footer).
fn register_settled_handler(
    api: &mut maho_ext_api::ExtensionApi,
    wiring: std::sync::Arc<tokio::sync::Mutex<MemoryWiring>>,
    handle: std::sync::Arc<maho_ext_api::ExtensionApi>,
    now: std::sync::Arc<dyn Fn() -> f64 + Send + Sync>,
    warn: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
) {
    api.on(
        maho_ext_api::EventKind::AgentSettled,
        std::sync::Arc::new(move |_, context| {
            let wiring = wiring.clone();
            let api = handle.clone();
            let now = now.clone();
            let warn = warn.clone();
            Box::pin(async move {
                let entries = context
                    .session_manager
                    .get_branch()
                    .into_iter()
                    .map(|entry| entry.data)
                    .collect::<Vec<_>>();
                let provenance = context.session_manager.fork_provenance();
                let mut ui = NativeFooter(context.ui.clone());
                let mut locked = wiring.lock().await;
                if let Some(prov) = provenance {
                    locked.record_provenance(context.session_manager.session_id(), prov);
                }
                locked
                    .on_settled_with_provenance(
                        context.session_manager.session_id(),
                        &entries,
                        &api,
                        now() as i64,
                        Some(&mut ui),
                        |session, error| {
                            warn(&format!("journal reconcile failed: {session}: {error}"))
                        },
                    )
                    .map_err(maho_ext_api::ExtensionFailure::new)?;
                Ok(maho_ext_api::EventResult::None)
            })
        }),
    );
}
impl MemoryWiring {
    pub fn register_native(
        wiring: std::sync::Arc<tokio::sync::Mutex<Self>>,
        component: &std::sync::Arc<crate::index::MemoryComponent>,
        api: &mut maho_ext_api::ExtensionApi,
        options: crate::wiring_types::NativeMemoryWiringOptions,
        register_static: impl FnOnce(&mut maho_ext_api::ExtensionApi),
    ) -> Result<bool, String> {
        let activity_wiring = wiring.clone();
        api.on(
            maho_ext_api::EventKind::Input,
            std::sync::Arc::new(move |event, ctx| {
                let wiring = activity_wiring.clone();
                let input = match event {
                    maho_ext_api::ExtensionEvent::Input(input)
                        if input.source != maho_ext_api::InputSource::Extension
                            && !input.text.trim().is_empty() =>
                    {
                        Some(input.input_id.clone())
                    }
                    _ => None,
                };
                let session = ctx.session_manager.session_id().to_string();
                Box::pin(async move {
                    if let Some(input) = input {
                        wiring
                            .lock()
                            .await
                            .user_activity
                            .record_input(&input, &session);
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
        let activity_wiring = wiring.clone();
        api.on(
            maho_ext_api::EventKind::InputDisposition,
            std::sync::Arc::new(move |event, _| {
                let wiring = activity_wiring.clone();
                let input = match event {
                    maho_ext_api::ExtensionEvent::InputDisposition {
                        input_id,
                        disposition,
                    } => Some((
                        input_id.clone(),
                        match disposition {
                            maho_ext_api::InputDisposition::Started => "started",
                            maho_ext_api::InputDisposition::Queued => "queued",
                            _ => "rejected",
                        },
                    )),
                    _ => None,
                };
                Box::pin(async move {
                    if let Some((id, state)) = input {
                        wiring.lock().await.user_activity.disposition(&id, state);
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
        let handle = std::sync::Arc::new(maho_ext_api::ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        let bind_wiring = wiring.clone();
        let bind_handle = handle.clone();
        let initial = options.initial_status;
        let facts = options.facts;
        let reconcile = options.reconcile;
        let recovery = reconcile.clone();
        wiring
            .try_lock()
            .map_err(|_| "memory wiring is busy during registration".to_string())?
            .seams
            .reconcile_runs = Some(std::sync::Arc::new(move |identity| {
            let recovery = recovery.clone();
            let identity = identity.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(recovery(identity))
            })
            .join()
            .map_err(|_| "memory stale-holder reconciliation panicked".to_string())?
        }));
        let now = options.now.clone();
        let journal_warn = options.warn.clone();
        let shutdown_wiring = wiring.clone();
        let shutdown_now = options.now.clone();
        let shutdown_warn = options.warn.clone();
        // Pin `wiring-static.ts:96-111`: the settled journal/facts/footer handler is registered before
        // the reflection trigger (:183) and dream trigger (:184), so a settle journals its final turn first.
        let settled_wiring = wiring.clone();
        let settled_handle = handle.clone();
        let settled_now = options.now.clone();
        let settled_warn = options.warn.clone();
        let unbind_wiring = wiring.clone();
        let register_static = move |api: &mut maho_ext_api::ExtensionApi| {
            register_session_reset_handler(api, unbind_wiring);
            register_settled_handler(
                api,
                settled_wiring,
                settled_handle,
                settled_now,
                settled_warn,
            );
            register_static(api);
        };
        let registered = component.register(
            api,
            crate::index::MemoryComponentHooks {
                after_bind: std::sync::Arc::new(move |session, identity, context| {
                    let wiring = bind_wiring.clone();
                    let api = bind_handle.clone();
                    let identity = identity.clone();
                    let facts = facts(&identity, context);
                    let reconcile = reconcile.clone();
                    let now = now.clone();
                    let warn = journal_warn.clone();
                    let initial = initial.clone();
                    Box::pin(async move {
                        let entries = context
                            .session_manager
                            .get_branch()
                            .into_iter()
                            .map(|entry| entry.data)
                            .collect::<Vec<_>>();
                        let mut wiring = wiring.lock().await;
                        let mut ui = NativeFooter(context.ui.clone());
                        wiring.live_host = Some((api.clone(), context.ui.clone()));
                        let mut live = NativeBindLive {
                            api: maho_ext_api::ExtensionApi::new(
                                api.registered.clone(),
                                api.profile.clone(),
                                api.events.clone(),
                                api.runtime.clone(),
                            ),
                            context,
                            initial,
                            warn: warn.clone(),
                        };
                        let mut api = maho_ext_api::ExtensionApi::new(
                            api.registered.clone(),
                            api.profile.clone(),
                            api.events.clone(),
                            api.runtime.clone(),
                        );
                        let work = reconcile(identity.clone());
                        wiring
                            .after_bind(
                                &mut api,
                                MemoryAfterBindInput {
                                    session_id: session,
                                    identity,
                                    entries: &entries,
                                    now_ms: now() as i64,
                                },
                                Some(&mut ui),
                                Some(&mut live),
                                || work,
                                facts?,
                                |session, error| {
                                    warn(&format!("journal reconcile failed: {session}: {error}"))
                                },
                            )
                            .await
                    })
                }),
                shutdown: std::sync::Arc::new(move |event, context, deadline| {
                    let wiring = shutdown_wiring.clone();
                    let now = shutdown_now.clone();
                    let warn = shutdown_warn.clone();
                    Box::pin(async move {
                        let maho_ext_api::ExtensionEvent::SessionShutdown(event) = event else {
                            return Err("memory shutdown hook received another event".into());
                        };
                        let reason = match event.reason {
                            maho_ext_api::SessionReason::Quit => {
                                crate::shutdown_drain::ShutdownReason::Quit
                            }
                            maho_ext_api::SessionReason::Reload => {
                                crate::shutdown_drain::ShutdownReason::Reload
                            }
                            maho_ext_api::SessionReason::New => {
                                crate::shutdown_drain::ShutdownReason::New
                            }
                            maho_ext_api::SessionReason::Resume => {
                                crate::shutdown_drain::ShutdownReason::Resume
                            }
                            maho_ext_api::SessionReason::Fork => {
                                crate::shutdown_drain::ShutdownReason::Fork
                            }
                            maho_ext_api::SessionReason::Startup => {
                                crate::shutdown_drain::ShutdownReason::Reload
                            }
                        };
                        let provenance = context.session_manager.fork_provenance();
                        if let Some(prov) = provenance {
                            wiring
                                .lock()
                                .await
                                .record_provenance(context.session_manager.session_id(), prov);
                        }
                        wiring
                            .lock()
                            .await
                            .on_session_shutdown(
                                reason,
                                context.session_manager.session_id(),
                                deadline,
                                now.as_ref(),
                                &mut |step, error| {
                                    warn(&format!(
                                        "memory shutdown {step}: {}",
                                        error.unwrap_or("deadline reached")
                                    ))
                                },
                            )
                            .await;
                        Ok(())
                    })
                }),
                clear_status: std::sync::Arc::new(|context| {
                    context
                        .ui
                        .set_status(crate::status::MEMORY_STATUS_KEY, None)
                }),
                warn: options.warn.clone(),
            },
            register_static,
        )?;
        if !registered {
            return Ok(false);
        }
        // Pull side of the RPC bridge (pin `wiring.ts:111` registerRpc in registerStatic).
        let rpc_component = component.clone();
        let rpc_now = options.now.clone();
        // Pin `wiring.ts` registerStatic: one configured applier (people routing read from the current
        // memory settings on every call) serves the RPC and is published for the onboarding component.
        let apply_component = component.clone();
        let apply_sessions = component.clone();
        let apply_facts = crate::facts_apply_rpc::create_configured_facts_applier(
            std::sync::Arc::new(
                move || serde_json::json!({"memory":apply_component.settings().unwrap_or_default()}),
            ),
            std::sync::Arc::new(move |session| {
                apply_sessions
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(session?)?
                    .context
                    .clone()
            }),
        );
        crate::facts_apply_rpc::publish_session_facts_applier(Some(apply_facts.clone()));
        wiring.try_lock().map_err(|error|format!("memory wiring locked during registration: {error}"))?.reflection_live.register_rpc(api,std::sync::Arc::new(move||->std::collections::BTreeMap<String,crate::context::MemoryIdentityContext>{
            rpc_component.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|(_,state)|state.enabled).filter_map(|(session,state)|state.context.clone().map(|context|(session.clone(),context))).collect()
        }),std::sync::Arc::new(move||rpc_now() as i64),Some(apply_facts)).map_err(|error|error.to_string())?;
        let state = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
            std::path::PathBuf,
            crate::sync_runner::SyncRepoStatus,
        >::new()));
        {
            let source = state.clone();
            let read: crate::sync_status_cached::SyncStatusReader =
                std::sync::Arc::new(move |repo| {
                    source
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(repo)
                        .cloned()
                        .map(crate::sync_status_cached::report)
                });
            let mut locked = wiring
                .try_lock()
                .map_err(|_| "memory wiring busy during sync registration")?;
            locked.set_sync_status(Some(read));
            let source = state.clone();
            locked
                .reflection_live
                .rpc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_status = Some(std::sync::Arc::new(move |repo| {
                source
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(repo)
                    .cloned()
            }));
        }
        if let Some(runner) = component.sync_runner() {
            let wiring = wiring.clone();
            let now = options.now.clone();
            let warn = options.warn.clone();
            let (changes, received) = std::sync::mpsc::channel::<std::path::PathBuf>();
            std::thread::Builder::new()
                .name("memory-sync-surface".into())
                .spawn(move || {
                    while let Ok(repo) = received.recv() {
                        if let Err(error) = wiring
                            .blocking_lock()
                            .on_sync_status_changed(&repo, now() as i64)
                        {
                            warn(&error);
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            runner.on_change(std::sync::Arc::new(move |repo, status| {
                {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(status) = status {
                        state.insert(repo.clone(), status);
                    } else {
                        state.remove(&repo);
                    }
                }
                if changes.send(repo).is_err() {
                    eprintln!("memory sync surface receiver closed");
                }
            }));
        }
        Ok(true)
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn after_bind<F: std::future::Future<Output = Result<(), String>>>(
        &mut self,
        api: &mut maho_ext_api::ExtensionApi,
        input: MemoryAfterBindInput<'_>,
        ui: Option<&mut (dyn MemoryFooterUi + Send)>,
        live: Option<&mut (dyn crate::wiring_reflection_live::ReflectionBindLive + Send)>,
        reconcile: impl FnOnce() -> F,
        facts: crate::facts_wiring::MemoryFactsWiringOptions,
        warn: impl FnOnce(&str, &memory_core::journal::store::JournalError),
    ) -> Result<(), String> {
        self.runtime
            .contexts
            .insert(input.session_id.into(), input.identity.clone());
        self.reflection_live.attach(input.session_id);
        self.maintenance.schedule(input.identity.clone());
        crate::policy_guard::register_memory_filesystem_policy(Some(api), Some(&input.identity))
            .map_err(|error| error.to_string())?;
        reconcile().await?;
        // Session-start reconciliation runs once per session; a holder that dies after it is only
        // recoverable from a later turn, which is what `recover_stale_holder_after_turn` does.
        self.user_activity.mark_reconciled(input.session_id);
        if !input.entries.is_empty() {
            self.runtime
                .journal_wiring_for(&input.identity)
                .reconcile_session(Some(input.session_id), input.entries, warn)
                .map_err(|error| error.to_string())?;
        }
        self.runtime
            .facts_wiring_for(&input.identity, || facts)
            .reconcile_extractor_for(Some(input.session_id));
        self.reflection_live.bind(
            input.session_id,
            input.identity,
            api,
            input.now_ms,
            ui.map(|ui| ui as &mut dyn MemoryFooterUi),
            live.map(|live| live as &mut dyn crate::wiring_reflection_live::ReflectionBindLive),
        )
    }
    pub fn flush_skills_usage(
        &mut self,
        now: &dyn Fn() -> String,
        aborted: Option<&dyn Fn() -> bool>,
        warn: &dyn Fn(&str),
    ) {
        for tracker in self
            .skills_usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values_mut()
        {
            if aborted.is_some_and(|aborted| aborted()) {
                return;
            }
            tracker.flush(now, aborted, warn);
        }
    }
    pub fn clear_status(&mut self) {
        self.reflection_live.clear_status();
    }
    /// Launch hook (pin `wiring-reflection-live.ts` onReflectionLaunched). Returns true when the
    /// caller must start the footer ticker via [`spawn_footer_ticker`].
    pub fn on_reflection_launched(
        &mut self,
        identity: &str,
        run: &str,
        details: crate::status_active_runs::ActiveReflectionRunDetails,
        now: i64,
        warn: &dyn Fn(&str),
    ) -> bool {
        let Some((api, ui)) = self.live_host.clone() else {
            self.reflection_live
                .active_runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .start(identity, run, details);
            return false;
        };
        if let Err(error) = self.reflection_live.on_reflection_launched(
            identity,
            run,
            details,
            &api,
            now,
            Some(&mut NativeFooter(ui)),
        ) {
            warn(&format!("memory rpc sync failed: {error}"));
        }
        let start = self.reflection_live.footer.is_animating() && !self.footer_ticker;
        if start {
            self.footer_ticker = true;
        }
        start
    }
    /// Live completion hook (pin `wiring-reflection-live.ts` onLiveReflectionCompleted).
    pub fn on_live_reflection_completed(
        &mut self,
        identity: &str,
        run: &str,
        now: i64,
    ) -> Result<(), String> {
        let Some((api, ui)) = self.live_host.clone() else {
            self.reflection_live
                .active_runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settle(identity, run);
            return Ok(());
        };
        self.reflection_live.on_live_reflection_completed(
            identity,
            run,
            &api,
            now,
            Some(&mut NativeFooter(ui)),
        )
    }
    /// Memory-write hook (pin `wiring.ts:133` onMemoryWrite: reflectionLive.syncRpc).
    pub fn sync_rpc(&mut self, now: i64) -> Result<(), String> {
        let Some((api, _)) = self.live_host.clone() else {
            return Ok(());
        };
        self.reflection_live.sync_rpc(&api, now)
    }
    /// Installs the sync-status reader the footer, the notices and [`Self::on_sync_status_changed`] read.
    pub fn set_sync_status(
        &mut self,
        sync_status: Option<crate::sync_status_cached::SyncStatusReader>,
    ) {
        self.seams.sync_status = sync_status.clone();
        self.reflection_live.set_sync_status(sync_status);
    }
    /// A repository's sync failure appeared, changed kind or cleared: redraw the footer and tell the person.
    ///
    /// Only the active session's repository redraws a footer or pushes the desktop snapshot; any other
    /// repository still feeds the notifier, so its notice state follows every transition.
    pub fn on_sync_status_changed(
        &mut self,
        repo: &std::path::Path,
        now: i64,
    ) -> Result<(), String> {
        let session = self.reflection_live.session().map(str::to_owned);
        let active_repo = session
            .as_deref()
            .and_then(|session| self.runtime.resolve_context(session))
            .map(|context| context.identity_paths.repo.as_path() == repo)
            .unwrap_or(false);
        let host = self.live_host.clone();
        let notice = if active_repo {
            host.as_ref().map(|(_, ui)| NativeSyncNotice(ui.clone()))
        } else {
            None
        };
        let notifier = self.reflection_live.sync_notifier.clone();
        let mut apply = || -> Result<(), String> {
            let (Some((api, ui)), Some(session)) = (host.as_ref(), session.as_deref()) else {
                return Ok(());
            };
            // Redrawing the footer and pushing the desktop snapshot is exactly what a settle does.
            self.reflection_live
                .on_settled(session, api, now, Some(&mut NativeFooter(ui.clone())))
        };
        crate::sync_status_cached::deliver_sync_status_change(
            crate::sync_status_cached::SyncStatusChange {
                repo,
                is_active_repo: active_repo,
                notifier: notifier.as_deref(),
                ui: notice
                    .as_ref()
                    .map(|notice| notice as &dyn crate::sync_status_cached::SyncNoticeUi),
                apply: &mut apply,
            },
        )
    }
    /// Settles a reflection holder that provably died after this session's initial reconciliation (#10377).
    ///
    /// Each later turn settles such a holder once and reports a holder whose liveness cannot be proven
    /// once per session and run; a live owner is never disturbed.
    pub fn recover_stale_holder_after_turn(
        &mut self,
        session: &str,
        identity: &crate::context::MemoryIdentityContext,
    ) {
        if !self.user_activity.has(session) || !self.user_activity.has_reconciled(session) {
            return;
        }
        let reconcile_runs = self.seams.reconcile_runs.clone();
        if reconcile_runs.is_none() {
            return;
        }
        // A holder that one reconciliation could not release is reported, not reconciled every turn.
        let settled = std::cell::RefCell::new(std::mem::take(&mut self.stale_holder_reconciles));
        let should_reconcile = |run_id: &str| {
            let key = format!("{session}:{run_id}");
            let mut settled = settled.borrow_mut();
            if settled.contains(&key) {
                return false;
            }
            settled.insert(key);
            true
        };
        let reconcile = || reconcile_runs.as_ref().expect("checked reconcile seam")(identity);
        let recovery = crate::reflection_stale_active_sync::recover_stale_reflection_holder(
            crate::reflection_stale_active_sync::StaleHolderRecoveryInput {
                reflection_dir: &identity.identity_paths.reflection,
                reconcile: &reconcile,
                seams: None,
                should_reconcile: Some(&should_reconcile),
            },
        );
        self.stale_holder_reconciles = settled.into_inner();
        let (Some(notice), Some(run_id)) = (recovery.notice, recovery.verdict.run_id()) else {
            return;
        };
        let key = format!("{session}:{run_id}");
        if !self.stale_lock_notices_shown.insert(key) {
            return;
        }
        if let Some((_, ui)) = self.live_host.clone() {
            ui.notify(&notice, maho_ext_api::NotificationType::Warning);
        }
    }
    pub fn record_provenance(&mut self, session: &str, provenance: maho_ext_api::ForkProvenance) {
        self.provenance_by_session
            .insert(session.to_owned(), provenance);
    }
    pub fn on_settled(
        &mut self,
        session: &str,
        entries: &[serde_json::Value],
        api: &maho_ext_api::ExtensionApi,
        now: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
        warn: impl FnOnce(&str, &memory_core::journal::store::JournalError),
    ) -> Result<(), String> {
        self.on_settled_with_provenance(session, entries, api, now, ui, warn)
    }
    pub fn on_settled_with_provenance(
        &mut self,
        session: &str,
        entries: &[serde_json::Value],
        api: &maho_ext_api::ExtensionApi,
        now: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
        warn: impl FnOnce(&str, &memory_core::journal::store::JournalError),
    ) -> Result<(), String> {
        let Some(identity) = self.runtime.resolve_context(session).cloned() else {
            return Ok(());
        };
        // A holder that died after this session reconciled would keep the reflection lock for good, so each
        // later settled turn settles a provably dead one before anything else queues behind it.
        self.recover_stale_holder_after_turn(session, &identity);
        if !entries.is_empty() {
            self.runtime
                .journal_wiring_for(&identity)
                .reconcile_session(Some(session), entries, warn)
                .map_err(|error| error.to_string())?;
            if let Some(facts) = self.runtime.existing_facts_wiring(&identity.identity) {
                if let Some(prov) = self.provenance_by_session.get(session) {
                    facts.set_provenance(session, Some(prov.clone()));
                }
                facts.on_settled(session);
            }
        }
        self.reflection_live.on_settled(session, api, now, ui)
    }
    pub fn shutdown(&mut self, session: &str) {
        let identity = self
            .runtime
            .resolve_context(session)
            .map(|context| context.identity.clone());
        self.reflection_live.shutdown(identity.as_deref());
    }
    /// Drops the session's bound context so later settles resolve nothing (pin `wiring-runtime.ts:51-52`).
    pub fn unbind(&mut self, session: &str) {
        let identity = self
            .runtime
            .resolve_context(session)
            .map(|context| context.identity.clone());
        if let Some(identity) = identity
            && let Some(facts) = self.runtime.existing_facts_wiring(&identity)
        {
            facts.set_provenance(session, None);
        }
        self.runtime.contexts.remove(session);
        self.reflection_live.unbind(session);
        self.user_activity.reset(session);
        self.provenance_by_session.remove(session);
    }
    pub async fn on_session_shutdown(
        &mut self,
        reason: crate::shutdown_drain::ShutdownReason,
        session: &str,
        deadline: f64,
        now: &(dyn Fn() -> f64 + Sync),
        warn: &mut (dyn FnMut(&str, Option<&str>) + Send),
    ) {
        // Upstream `onSessionShutdown` calls `w.dispose()` synchronously and never awaits `settled()`: an in-flight
        // maintenance run is cancelled and winds down in the background instead of being joined on the shutdown path.
        self.maintenance.dispose();
        // pin `wiring.ts` raceDetached "recall-store-settle": after "status-settle", before the reflection-live shutdown,
        // so the recall term-store save finishes. A save that outlives the deadline is left running.
        if let Some(settle) = self.seams.recall_store_settle.clone() {
            let remaining = std::time::Duration::from_secs_f64(
                ((deadline - now()).max(0.0) / 1000.0).min(u64::MAX as f64),
            );
            let work = tokio::task::spawn_blocking(move || settle());
            if tokio::time::timeout(remaining, work).await.is_err() {
                warn("recall-store-settle", None);
            }
        }
        self.shutdown(session);
        let identity = self
            .runtime
            .resolve_context(session)
            .map(|context| context.identity.clone());
        if let Some(identity) = identity
            && let Some(facts) = self.runtime.existing_facts_wiring(&identity)
        {
            facts.cancel_active().await;
        }
        let evaluators = std::mem::take(&mut self.evaluators);
        let evaluators = {
            let mut drain = crate::shutdown_drain::create_shutdown_drain(MemoryShutdownSteps {
                wiring: self,
                now,
            });
            for evaluator in evaluators {
                drain.register_evaluator(evaluator);
            }
            drain.run(reason, session, deadline, now, warn).await;
            std::mem::take(&mut drain.evaluators)
        };
        self.evaluators = evaluators;
        self.unbind(session);
    }
    pub fn register_shutdown_evaluator(
        &mut self,
        evaluator: Box<dyn crate::shutdown_drain::ShutdownEvaluator>,
    ) {
        self.evaluators.push(evaluator);
    }
    pub fn register_dream_triggers(
        &mut self,
        api: &mut maho_ext_api::ExtensionApi,
        dream: std::sync::Arc<crate::dream_trigger::DreamTriggerWiring>,
    ) {
        dream.register(api);
        self.register_shutdown_evaluator(dream.shutdown_evaluator());
    }
}
struct MemoryShutdownSteps<'a> {
    wiring: &'a mut MemoryWiring,
    now: &'a (dyn Fn() -> f64 + Sync),
}
impl crate::shutdown_drain::ShutdownDrainSteps for MemoryShutdownSteps<'_> {
    fn flush_journal<'a>(
        &'a mut self,
        session: &'a str,
        signal: crate::shutdown_drain::ShutdownSignal,
    ) -> crate::shutdown_drain::ShutdownWork<'a> {
        Box::pin(async move {
            let Some(identity) = self.wiring.runtime.resolve_context(session).cloned() else {
                return Ok(());
            };
            self.wiring
                .runtime
                .journal_wiring_for(&identity)
                .journal_for(session)
                .flush(Some(&|| signal.aborted()))
                .map_err(|error| error.to_string())
        })
    }
    fn enqueue_final_delta<'a>(
        &'a mut self,
        session: &'a str,
        signal: crate::shutdown_drain::ShutdownSignal,
    ) -> crate::shutdown_drain::ShutdownWork<'a> {
        Box::pin(async move {
            if signal.aborted() {
                return Ok(());
            }
            let Some(identity) = self
                .wiring
                .runtime
                .resolve_context(session)
                .map(|context| context.identity.clone())
            else {
                return Ok(());
            };
            let provenance = self.wiring.provenance_by_session.get(session).cloned();
            if let Some(facts) = self.wiring.runtime.existing_facts_wiring(&identity) {
                if let Some(prov) = provenance {
                    facts.set_provenance(session, Some(prov));
                }
                facts.enqueue_settled(session, Some(signal.facts_signal()));
            }
            Ok(())
        })
    }
    fn flush_skills_usage<'a>(
        &'a mut self,
        _: &'a str,
        signal: crate::shutdown_drain::ShutdownSignal,
    ) -> crate::shutdown_drain::ShutdownWork<'a> {
        Box::pin(async move {
            let now = chrono::DateTime::from_timestamp_millis((self.now)() as i64)
                .ok_or("Invalid shutdown clock")?
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let failure = std::cell::RefCell::new(None);
            self.wiring.flush_skills_usage(
                &|| now.clone(),
                Some(&|| signal.aborted()),
                &|message| {
                    *failure.borrow_mut() = Some(message.to_owned());
                },
            );
            failure.into_inner().map_or(Ok(()), Err)
        })
    }
    fn launch_facts<'a>(
        &'a mut self,
        session: &'a str,
        signal: crate::shutdown_drain::ShutdownSignal,
    ) -> crate::shutdown_drain::ShutdownWork<'a> {
        Box::pin(async move {
            if signal.aborted() {
                return Ok(());
            }
            let Some(identity) = self
                .wiring
                .runtime
                .resolve_context(session)
                .map(|context| context.identity.clone())
            else {
                return Ok(());
            };
            if let Some(facts) = self.wiring.runtime.existing_facts_wiring(&identity) {
                facts
                    .launch_if_threshold_met(Some(signal.facts_signal()))
                    .await;
            }
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Evaluator(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl crate::shutdown_drain::ShutdownEvaluator for Evaluator {
        fn evaluate<'a>(
            &'a mut self,
            input: crate::shutdown_drain::ShutdownEvaluatorInput<'a>,
        ) -> crate::shutdown_drain::ShutdownWork<'a> {
            Box::pin(async move {
                assert_eq!(input.reason, crate::shutdown_drain::ShutdownReason::Quit);
                assert_eq!(input.session_id, "session");
                assert!(!input.signal.aborted());
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }
    }
    #[tokio::test]
    async fn registered_evaluator_survives_reload_and_runs_on_each_quit() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut wiring = create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        });
        wiring.register_shutdown_evaluator(Box::new(Evaluator(calls.clone())));
        wiring
            .on_session_shutdown(
                crate::shutdown_drain::ShutdownReason::Reload,
                "session",
                1500.0,
                &|| 0.0,
                &mut |step, error| panic!("{step}: {error:?}"),
            )
            .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        for count in 1..=2 {
            wiring
                .on_session_shutdown(
                    crate::shutdown_drain::ShutdownReason::Quit,
                    "session",
                    1500.0,
                    &|| 0.0,
                    &mut |step, error| panic!("{step}: {error:?}"),
                )
                .await;
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), count);
        }
    }
    #[tokio::test]
    async fn registered_dream_triggers_run_shutdown_dream_only_on_quit() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let seen = calls.clone();
        let dream = std::sync::Arc::new(crate::dream_trigger::DreamTriggerWiring::new(
            crate::dream_trigger::DreamTriggerOptions {
                resolve_session: std::sync::Arc::new(|_| None),
                resolve_active_session: std::sync::Arc::new(|| None),
                resolve_settings: std::sync::Arc::new(|_| Default::default()),
                launch: std::sync::Arc::new(move |session, origin, request, signal| {
                    seen.lock()
                        .unwrap()
                        .push((session, origin, request.deadline_at, signal));
                    Box::pin(async {
                        Ok(crate::dream_trigger::DreamFireOutcome::Fired {
                            run_id: "run".into(),
                            status: "active".into(),
                        })
                    })
                }),
                warn: std::sync::Arc::new(|error| panic!("{error}")),
            },
        ));
        let mut wiring = create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        });
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        wiring.register_dream_triggers(&mut api, dream);
        assert!(
            api.registered
                .handlers
                .contains_key(&maho_ext_api::EventKind::SessionAbort)
        );
        wiring
            .on_session_shutdown(
                crate::shutdown_drain::ShutdownReason::Reload,
                "session",
                1500.0,
                &|| 0.0,
                &mut |step, error| panic!("{step}: {error:?}"),
            )
            .await;
        assert!(calls.lock().unwrap().is_empty());
        wiring
            .on_session_shutdown(
                crate::shutdown_drain::ShutdownReason::Quit,
                "session",
                1500.0,
                &|| 0.0,
                &mut |step, error| panic!("{step}: {error:?}"),
            )
            .await;
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "session");
        assert_eq!(calls[0].1, memory_core::reflection::DreamOrigin::Shutdown);
        assert_eq!(calls[0].2, Some(1500.0));
        assert!(calls[0].3.as_ref().unwrap().is_aborted());
    }
    #[tokio::test]
    async fn bind_reconciles_before_journal_and_facts_then_registers_policy() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let identity = crate::context::MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            crate::binding::create_memory_binding("agent", "repo", 0.0),
        );
        let mut wiring = create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        });
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let entries = vec![
            serde_json::json!({"type":"message","id":"one","message":{"role":"user","content":"hello"}}),
        ];
        let facts = crate::facts_wiring::MemoryFactsWiringOptions {
            identity: "agent".into(),
            identity_paths: paths.clone(),
            facts_enabled: Box::new(|| false),
            debounce_settles: Box::new(|| 4),
            extractor: None,
            now: None,
            warn: std::sync::Arc::new(|message| panic!("{message}")),
        };
        wiring
            .after_bind(
                &mut api,
                MemoryAfterBindInput {
                    session_id: "session",
                    identity,
                    entries: &entries,
                    now_ms: 0,
                },
                None,
                None,
                || async {
                    assert!(!paths.transcripts.join("session").exists());
                    Ok(())
                },
                facts,
                |_, error| panic!("{error}"),
            )
            .await
            .unwrap();
        assert_eq!(api.registered.filesystem_policies.len(), 1);
        assert!(wiring.runtime.resolve_context("session").is_some());
        assert!(paths.transcripts.join("session").exists());
        assert!(!paths.repo.exists());
    }
    #[tokio::test]
    async fn settled_reconciles_journal_before_facts_delta_and_empty_branch_does_not_enqueue() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let identity = crate::context::MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            crate::binding::create_memory_binding("agent", "repo", 0.0),
        );
        let mut wiring = create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        });
        wiring
            .runtime
            .contexts
            .insert("session".into(), identity.clone());
        wiring.runtime.facts_wiring_for(&identity, || {
            crate::facts_wiring::MemoryFactsWiringOptions {
                identity: "agent".into(),
                identity_paths: paths.clone(),
                facts_enabled: Box::new(|| true),
                debounce_settles: Box::new(|| 4),
                extractor: None,
                now: Some(std::sync::Arc::new(|| 0)),
                warn: std::sync::Arc::new(|error| panic!("{error}")),
            }
        });
        let api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        wiring
            .on_settled("session", &[], &api, 0, None, |_, error| panic!("{error}"))
            .unwrap();
        assert!(!paths.transcripts.join("session").exists());
        let entries = vec![
            serde_json::json!({"type":"message","id":"user","message":{"role":"user","content":"question"}}),
            serde_json::json!({"type":"message","id":"assistant","message":{"role":"assistant","content":"answer"}}),
        ];
        wiring
            .on_settled("session", &entries, &api, 0, None, |_, error| {
                panic!("{error}")
            })
            .unwrap();
        let pending = wiring
            .runtime
            .existing_facts_wiring("agent")
            .unwrap()
            .reconcile_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].conversation_id, "session");
        wiring
            .on_settled("session", &entries, &api, 0, None, |_, error| {
                panic!("{error}")
            })
            .unwrap();
        assert_eq!(
            wiring
                .runtime
                .existing_facts_wiring("agent")
                .unwrap()
                .reconcile_pending()
                .len(),
            1
        );
    }

    #[test]
    fn test_unbind_clears_facts_provenance_entry() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let identity = crate::context::MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            crate::binding::create_memory_binding("agent", "repo", 0.0),
        );
        let mut wiring = create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        });
        wiring
            .runtime
            .contexts
            .insert("session".into(), identity.clone());
        wiring.runtime.facts_wiring_for(&identity, || {
            crate::facts_wiring::MemoryFactsWiringOptions {
                identity: "agent".into(),
                identity_paths: paths.clone(),
                facts_enabled: Box::new(|| true),
                debounce_settles: Box::new(|| 4),
                extractor: None,
                now: Some(std::sync::Arc::new(|| 0)),
                warn: std::sync::Arc::new(|error| panic!("{error}")),
            }
        });
        let prov = maho_ext_api::ForkProvenance {
            parent_session: "parent".into(),
            inherited_through_entry_id: Some("entry-1".into()),
        };
        wiring
            .runtime
            .existing_facts_wiring("agent")
            .unwrap()
            .set_provenance("session", Some(prov.clone()));
        wiring.record_provenance("session", prov);
        assert!(wiring.provenance_by_session.contains_key("session"));
        assert!(
            wiring
                .runtime
                .existing_facts_wiring("agent")
                .unwrap()
                .provenance("session")
                .is_some()
        );

        wiring.unbind("session");

        assert!(!wiring.provenance_by_session.contains_key("session"));
        assert!(
            wiring
                .runtime
                .existing_facts_wiring("agent")
                .unwrap()
                .provenance("session")
                .is_none()
        );
        assert!(wiring.runtime.resolve_context("session").is_none());
    }
}
