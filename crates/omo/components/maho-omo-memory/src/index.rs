use crate::{
    binding::{MEMORY_BINDING_CUSTOM_TYPE, create_memory_binding},
    context::MemoryIdentityContext,
    supervisor::MemoryModuleSupervisor,
};
use maho_ext_api::{
    EventKind, EventResult, ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionUi,
    NotificationType,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
pub struct MemoryComponentOptions {
    pub env: BTreeMap<String, String>,
    pub load_config: Arc<dyn Fn() -> Result<serde_json::Value, String> + Send + Sync>,
    pub cwd: PathBuf,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub disabled: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Seam for the end-to-end encrypted sync runner (`sync_runner.rs`); `None` builds the default idle-without-session runner at registration.
    pub sync_runner: Option<Arc<dyn crate::sync_runner::SyncRunner>>,
}
pub struct MemorySessionState {
    pub enabled: bool,
    pub context: Option<MemoryIdentityContext>,
    pub memory_status_attempted: bool,
    pub restart_notified: bool,
    /// The repository this session holds the sync runner for; released with the binding.
    pub sync_repo: Option<PathBuf>,
    ui: Option<Arc<dyn ExtensionUi>>,
}
pub struct MemoryComponent {
    options: MemoryComponentOptions,
    sync_runner: Mutex<Option<Arc<dyn crate::sync_runner::SyncRunner>>>,
    pub sessions: Mutex<BTreeMap<String, MemorySessionState>>,
    pub supervisor: MemoryModuleSupervisor,
}
pub type MemoryHookFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>;
pub type MemoryBindHook = Arc<
    dyn for<'a> Fn(&'a str, &'a MemoryIdentityContext, &'a ExtensionContext) -> MemoryHookFuture<'a>
        + Send
        + Sync,
>;
pub type MemoryShutdownHook = Arc<
    dyn for<'a> Fn(
            &'a maho_ext_api::ExtensionEvent,
            &'a ExtensionContext,
            f64,
        ) -> MemoryHookFuture<'a>
        + Send
        + Sync,
>;
pub struct MemoryComponentHooks {
    pub after_bind: MemoryBindHook,
    pub shutdown: MemoryShutdownHook,
    pub clear_status: Arc<dyn Fn(&ExtensionContext) + Send + Sync>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}
pub fn is_memory_child_process(env: &BTreeMap<String, String>) -> bool {
    ["SENPI_MEMORY_REFLECTION", "SENPI_MEMORY_FACTS"]
        .iter()
        .any(|key| env.get(*key).is_some_and(|value| value == "1"))
}
impl MemoryComponent {
    pub fn new(options: MemoryComponentOptions) -> Arc<Self> {
        Arc::new(Self {
            sync_runner: Mutex::new(options.sync_runner.clone()),
            options,
            sessions: Mutex::new(BTreeMap::new()),
            supervisor: Default::default(),
        })
    }
    pub(crate) fn settings(&self) -> Result<serde_json::Value, String> {
        let config = (self.options.load_config)()?;
        crate::reflection_settings::resolve_memory_settings(config.get("memory"))
    }
    fn enabled(&self, settings: &serde_json::Value) -> bool {
        settings["enabled"] == true
            && !is_memory_child_process(&self.options.env)
            && !(self.options.disabled)()
    }
    pub fn sync_runner(&self) -> Option<Arc<dyn crate::sync_runner::SyncRunner>> {
        self.sync_runner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Starts releasing this session's sync hold; the handle resolves when the repo's engine has closed.
    fn release_sync(&self, state: &mut MemorySessionState) -> Option<tokio::task::JoinHandle<()>> {
        let repo = state.sync_repo.take()?;
        let runner = self.sync_runner()?;
        let handle = tokio::runtime::Handle::try_current().ok()?;
        Some(handle.spawn(async move {
            let _ = runner.release(&repo).await;
        }))
    }
    fn release(&self, state: &mut MemorySessionState) {
        let _ = self.release_sync(state);
        if state.context.take().is_some() {
            self.supervisor.release();
        }
    }
    pub fn register(
        self: &Arc<Self>,
        api: &mut ExtensionApi,
        hooks: MemoryComponentHooks,
        register_static: impl FnOnce(&mut ExtensionApi),
    ) -> Result<bool, String> {
        let boot = self.settings()?;
        if !self.enabled(&boot) {
            return Ok(false);
        }
        if !crate::capabilities::has_memory_capabilities(api) {
            return Ok(false);
        }
        if self.options.sync_runner.is_none() {
            struct WarnLogger(Arc<dyn Fn(&str) + Send + Sync>);
            impl crate::sync_runner::SyncRunnerLogger for WarnLogger {
                fn info(&self, _: &str, _: &str) {}
                fn warn(&self, message: &str, _: &str) {
                    (self.0)(message);
                }
                fn error(&self, message: &str, _: &str, _: &str) {
                    (self.0)(message);
                }
            }
            *self
                .sync_runner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
                crate::sync_runner::create_sync_runner(crate::sync_runner::SyncRunnerOptions {
                    logger: Arc::new(WarnLogger(hooks.warn.clone())),
                    resolve_session: {
                        let source =
                            crate::device_session::account_source::create_cli_account_source(
                                None,
                                &self.options.env,
                                None,
                            )
                            ?;
                        let pins = PathBuf::from(
                            self.options
                                .env
                                .get("HOME")
                                .cloned()
                                .unwrap_or_else(maho_core::config::home_dir),
                        )
                        .join(".omo/sync-device");
                        let resolver =
                            crate::device_session::resolver::create_device_session_resolver(
                                source,
                                pins,
                                Arc::new(
                                    crate::device_session::account_session::open_account_session,
                                ),
                            );
                        let project_dir = self.options.cwd.clone();
                        let profile = self
                            .settings()?
                            .get("agent")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned);
                        Some(Arc::new(move |mut request: crate::device_session::resolver::SyncSessionRequest| {
                            request.space.profile = profile.clone();
                            request.space.project_dir = project_dir.clone();
                            resolver(request)
                        }) as crate::sync_runner::SyncSessionResolver)
                    },
                    open: None,
                    close_wait: None,
                }),
            );
        }
        crate::native_badge::register_native_badge(api);
        let sweep_root = memory_core::identity::layout::resolve_memory_root(
            &self.options.env,
            &self.options.cwd,
        );
        let sweep_now = chrono::Utc::now().timestamp_millis();
        let _ = crate::transient_identity::sweep_transient_run_roots(
            &sweep_root,
            sweep_now,
            24 * 60 * 60 * 1000,
        );
        let _ = crate::transient_identity::sweep_abandoned_identity_roots(
            &sweep_root,
            sweep_now,
            7 * 24 * 60 * 60 * 1000,
        );
        register_static(api);
        let this = self.clone();
        let subscription = Arc::new(Mutex::new(Some(api.events.on(
            "config-watch:reloaded",
            Arc::new(move |payload| {
                if payload["registrationId"] != "omo" {
                    return;
                }
                let Ok(settings) = this.settings() else {
                    return;
                };
                let enabled = this.enabled(&settings);
                for state in this
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .values_mut()
                {
                    if state.enabled != enabled && !state.restart_notified {
                        state.restart_notified = true;
                        if let Some(ui) = &state.ui {
                            ui.notify(
                                "restart required to apply memory config change",
                                NotificationType::Warning,
                            );
                        }
                    }
                }
            }),
        ))));
        let this = self.clone();
        let sync_runner = self.sync_runner();
        let handle = ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        );
        let clear = hooks.clear_status.clone();
        let bind = hooks.after_bind;
        let warn = hooks.warn;
        api.on(EventKind::SessionStart,Arc::new(move |_,context|{
            clear(context);let result=(||{
                let settings=this.settings()?;let enabled=this.enabled(&settings);let id=context.session_manager.session_id();let id=if id.is_empty(){"unknown-session"}else{id};
                let mut sessions=this.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(previous)=sessions.get_mut(id){this.release(previous);}
                sessions.insert(id.into(),MemorySessionState{enabled,context:None,memory_status_attempted:false,restart_notified:false,sync_repo:None,ui:Some(context.ui.clone())});
                if !enabled{return Ok(None);}
                let session_cwd=context.cwd.clone();
                let identity=if memory_core::identity::is_auto_agent_value(settings["agent"].as_str()) { memory_core::identity::resolve_auto_identity(&session_cwd,&this.options.env,if settings["auto_identity"]=="git" { memory_core::identity::AutoIdentityMode::Git } else { memory_core::identity::AutoIdentityMode::Path }) } else { memory_core::identity::resolve::resolve_memory_identity(settings["agent"].as_str(),&session_cwd,&this.options.env) }.map_err(|error|error.to_string())?;
                let entries=context.session_manager.get_entries();
                let recorded=entries.iter().rev().find_map(|entry|{let data=&entry.data;if entry.kind!="custom"||data["customType"]!=MEMORY_BINDING_CUSTOM_TYPE{return None;}Some(crate::binding::MemorySessionBinding{identity:data["data"]["identity"].as_str()?.into(),repo_path_hash:data["data"]["repoPathHash"].as_str()?.into(),bound_at:data["data"]["boundAt"].as_f64()?})});
                let adoption=crate::identity_adoption::adopt_session_identity(recorded.as_ref(),crate::identity_adoption::BoundIdentity{id:identity.id,paths:identity.paths.clone()},identity.paths.root.ancestors().nth(2).unwrap_or(&identity.paths.root),settings["agent"].as_str(),false);
                let adopted=match adoption{crate::identity_adoption::IdentityAdoption::Resolved(value)|crate::identity_adoption::IdentityAdoption::Rebound(value)=>value,crate::identity_adoption::IdentityAdoption::Conflict=>{context.ui.notify("memory identity conflict: session binding cannot be reproduced; restart or fork the session",NotificationType::Error);return Ok(None);}};
                let one_shot=!context.has_ui||context.session_manager.get_session_dir().is_none();
                let memory_root=memory_core::identity::layout::resolve_memory_root(&this.options.env,&session_cwd);
                let run_token=format!("{:x}-{}-{}",((this.options.now)() as u64).max(1),std::process::id(),memory_core::support::random::random_uuid().replace('-',""));
                let run_paths=crate::transient_identity::resolve_identity_run_paths(&adopted.id,adopted.paths.clone(),&memory_root,one_shot,Some(&run_token));
                let binding=create_memory_binding(&adopted.id,&run_paths.paths.repo.to_string_lossy(),(this.options.now)());let value=serde_json::json!({"identity":binding.identity,"repoPathHash":binding.repo_path_hash,"boundAt":binding.bound_at});
                let bound=MemoryIdentityContext::new(adopted.id,run_paths.paths,binding).with_durable_root(run_paths.durable_root).with_project_scope(crate::context::ProjectScope { memory_root, auto_identity: settings["auto_identity"].as_str().unwrap_or("project").to_owned() });
                let state=sessions.get_mut(id).ok_or("Inserted memory session is missing")?;state.context=Some(bound.clone());this.supervisor.acquire();state.sync_repo=Some(bound.identity_paths.repo.clone());let sync_enabled=settings["sync"]["enabled"]==true;drop(sessions);
                handle.append_entry(MEMORY_BINDING_CUSTOM_TYPE,Some(value)).map_err(|error|error.to_string())?;
                Ok::<_,String>(Some((id.to_owned(),bound,sync_enabled)))
            })();let bind=bind.clone();let warn=warn.clone();let sync_runner=sync_runner.clone();let context=context.clone();Box::pin(async move{
                if let Some((id,bound,sync_enabled))=result.map_err(ExtensionFailure::new)?{
                    // upstream `void syncRunner.acquire(...)`: detached, bind never waits on the engine.
                    if let Some(runner)=sync_runner{let identity=bound.identity.clone();let paths=bound.identity_paths.clone();tokio::spawn(async move{runner.acquire(&identity,&paths,sync_enabled).await;});}
                    // pin `memory/index.ts:133-137`: bind-time reconcile floats past session_start by design;
                    // run it inline up to its first await, then detach the rest with the same warn-on-error.
                    let mut task=Box::pin(async move{if let Err(error)=bind(&id,&bound,&context).await{warn(&format!("memory bind-time reconcile failed: {error}"));}});
                    if std::future::Future::poll(task.as_mut(),&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending(){tokio::spawn(task);}
                }
                Ok(EventResult::None)
            })
        }));
        let this = self.clone();
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |event, context| {
                let deadline = crate::shutdown_drain::shutdown_deadline_at(|| (this.options.now)());
                let this = this.clone();
                let shutdown = hooks.shutdown.clone();
                let clear = hooks.clear_status.clone();
                let subscription = subscription.clone();
                Box::pin(async move {
                    let result = shutdown(event, context, deadline).await;
                    let finalize = this
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(context.session_manager.session_id())
                        .and_then(|state| {
                            state
                                .context
                                .as_ref()
                                .filter(|identity| {
                                    identity.identity_paths.root != identity.durable_root
                                })
                                .map(|identity| crate::transient_identity::IdentityRunPaths {
                                    paths: identity.identity_paths.clone(),
                                    durable_root: identity.durable_root.clone(),
                                    transient_run_root:
                                        crate::transient_identity::transient_run_root_from_identity(
                                            &identity.identity_paths.root,
                                        ),
                                })
                        });
                    clear(context);
                    // upstream order: start the sync release, drop the session, await the engine close, then finalize the run.
                    let sync_release = this
                        .sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(context.session_manager.session_id())
                        .and_then(|mut state| {
                            let pending = this.release_sync(&mut state);
                            this.release(&mut state);
                            pending
                        });
                    if let Some(pending) = sync_release {
                        let _ = pending.await;
                    }
                    if let Some(run) = finalize {
                        let _ = crate::transient_identity::finalize_identity_run(&run);
                    }
                    drop(
                        subscription
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take(),
                    );
                    result
                        .map(|_| EventResult::None)
                        .map_err(ExtensionFailure::new)
                })
            }),
        );
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_child_sentinels_disable_memory() {
        for key in ["SENPI_MEMORY_REFLECTION", "SENPI_MEMORY_FACTS"] {
            assert!(is_memory_child_process(&BTreeMap::from([(
                key.into(),
                "1".into()
            )])));
            assert!(!is_memory_child_process(&BTreeMap::from([(
                key.into(),
                "true".into()
            )])));
        }
    }
    #[test]
    fn disabled_boot_loads_once_without_registering() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let component = MemoryComponent::new(MemoryComponentOptions {
            env: Default::default(),
            cwd: "/project".into(),
            now: Arc::new(|| 0.0),
            disabled: Arc::new(|| true),
            sync_runner: None,
            load_config: Arc::new(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(serde_json::json!({}))
            }),
        });
        let mut api = ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        assert!(
            !component
                .register(
                    &mut api,
                    MemoryComponentHooks {
                        after_bind: Arc::new(|_, _, _| panic!("disabled")),
                        shutdown: Arc::new(|_, _, _| panic!("disabled")),
                        clear_status: Arc::new(|_| panic!("disabled")),
                        warn: Arc::new(|_| panic!("disabled"))
                    },
                    |_| panic!("disabled")
                )
                .unwrap()
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(api.registered.handlers.is_empty());
    }
}
