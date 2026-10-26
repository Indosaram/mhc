use crate::{
    context::MemoryIdentityContext,
    memory_rpc_bridge::{MemoryRpcBridge, MemoryRpcBridgeDeps},
    status_active_runs::{ActiveReflectionRun, ActiveReflectionRunDetails, ActiveReflectionRuns},
    status_live::MemoryFooterUi,
    status_live_wiring::MemoryFooterStatusLive,
    sync_status_cached::{SyncStatusNotifier, SyncStatusReader},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
#[derive(Default)]
pub struct MemoryReflectionLiveWiring {
    pub active_runs: Arc<Mutex<ActiveReflectionRuns>>,
    pub rpc: Arc<Mutex<MemoryRpcBridge>>,
    pub footer: MemoryFooterStatusLive,
    session: Option<String>,
    contexts: BTreeMap<String, MemoryIdentityContext>,
    health_once: std::collections::BTreeSet<String>,
    /// The repository sync-status reader every footer snapshot and notice consults.
    pub sync_status: Option<SyncStatusReader>,
    /// The transition-only notifier built from that reader; a repo that keeps failing the same way stays silent.
    pub sync_notifier: Option<std::sync::Arc<SyncStatusNotifier>>,
}
/// Host live session for bind-time delivery (pin `wiring-reflection-live.ts` bind: liveSession +
/// refreshInitialStatus). One host object serves both the completion and the health seams.
pub trait ReflectionBindLive {
    fn refresh_initial_status(&mut self, identity: &MemoryIdentityContext);
    fn completions(&mut self)
    -> &mut dyn crate::worker::completion_delivery::ReflectionLiveSession;
    fn health(&mut self) -> &mut dyn crate::worker::health_alert::ReflectionHealthLiveSession;
}
struct Deps<'a> {
    contexts: &'a BTreeMap<String, MemoryIdentityContext>,
    runs: &'a ActiveReflectionRuns,
}
impl MemoryRpcBridgeDeps for Deps<'_> {
    fn resolve_context(&self, session: &str) -> Option<&MemoryIdentityContext> {
        self.contexts.get(session)
    }
    fn active_run(&self, identity: &str) -> Option<&ActiveReflectionRun> {
        self.runs.current(identity)
    }
}
/// Pull-side deps for `omo.memory.status`: contexts come from the component session map
/// (pin `wiring.ts:111` registerRpc(pi, resolveContext)) and the active run from the shared runs.
struct OwnedDeps {
    contexts: BTreeMap<String, MemoryIdentityContext>,
    runs: BTreeMap<String, ActiveReflectionRun>,
}
impl MemoryRpcBridgeDeps for OwnedDeps {
    fn resolve_context(&self, session: &str) -> Option<&MemoryIdentityContext> {
        self.contexts.get(session)
    }
    fn active_run(&self, identity: &str) -> Option<&ActiveReflectionRun> {
        self.runs.get(identity)
    }
}
pub type MemoryRpcContexts = Arc<dyn Fn() -> BTreeMap<String, MemoryIdentityContext> + Send + Sync>;
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
impl MemoryReflectionLiveWiring {
    pub fn register_rpc(
        &self,
        api: &mut maho_ext_api::ExtensionApi,
        contexts: MemoryRpcContexts,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
        apply_facts: Option<crate::facts_apply_rpc::MemoryFactsApplier>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let runs = self.active_runs.clone();
        crate::memory_rpc_bridge::register_memory_rpc_bridge(
            api,
            self.rpc.clone(),
            Arc::new(move || -> Box<dyn MemoryRpcBridgeDeps + Send> {
                let contexts = contexts();
                let active = lock(&runs);
                let runs: BTreeMap<String, ActiveReflectionRun> = contexts
                    .values()
                    .filter_map(|context| {
                        active
                            .current(&context.identity)
                            .map(|run| (context.identity.clone(), run.clone()))
                    })
                    .collect();
                Box::new(OwnedDeps { contexts, runs })
            }),
            now,
            apply_facts,
        )
    }
    /// Upstream builds a fresh reflection-live wiring (and footer) on every `register`, so a
    /// session attached after a non-final `session_shutdown` gets a live footer again.
    pub fn attach(&mut self, session: &str) {
        if self.footer.is_disposed() {
            self.footer = MemoryFooterStatusLive::default();
            self.footer.set_sync_status(self.sync_status.clone());
        }
        self.session = Some(session.into());
        lock(&self.rpc).attach(session);
    }
    /// Installs the sync-status reader the footer snapshots and the notices read (`MemoryWiringOptions.syncStatus`).
    pub fn set_sync_status(&mut self, sync_status: Option<SyncStatusReader>) {
        self.footer.set_sync_status(sync_status.clone());
        self.sync_notifier = sync_status
            .clone()
            .map(|read| std::sync::Arc::new(SyncStatusNotifier::new(read)));
        self.sync_status = sync_status;
    }
    /// The session whose footer and notices the live wiring currently serves.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }
    pub fn bind(
        &mut self,
        session: &str,
        context: MemoryIdentityContext,
        api: &maho_ext_api::ExtensionApi,
        now: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
        live: Option<&mut dyn ReflectionBindLive>,
    ) -> Result<(), String> {
        let mut ui = ui;
        let mut drained = false;
        if let Some(live) = live {
            live.refresh_initial_status(&context);
            let dir = context.identity_paths.reflection.join("completions");
            match crate::worker::completion_delivery::consume_pending_reflection_completions(
                &dir,
                &context.identity,
                live.completions(),
                now,
            ) {
                Ok(consumed) => {
                    {
                        let mut runs = lock(&self.active_runs);
                        for record in &consumed {
                            runs.settle(&context.identity, &record.run_id);
                        }
                    }
                    let once = &mut self.health_once;
                    crate::worker::health_alert::emit_reflection_health_alert(
                        &dir,
                        &context.identity,
                        Some(live.health()),
                        &mut |key: &str| once.insert(key.to_owned()),
                        now,
                    );
                    drained = true;
                }
                Err(error) => live.completions().warn(
                    "memory reflection completion drain failed",
                    &error.to_string(),
                ),
            }
        }
        self.contexts.insert(session.into(), context);
        let context = self.contexts.get(session);
        let active =
            context.is_some_and(|context| lock(&self.active_runs).is_active(&context.identity));
        self.footer.sync_active(
            context,
            Some(session),
            active,
            now,
            match ui.as_mut() {
                Some(ui) => Some(&mut **ui),
                None => None,
            },
        );
        if drained {
            self.footer.refresh(context, Some(session), active, now, ui);
        }
        self.sync_rpc(api, now)
    }
    pub fn on_reflection_launched(
        &mut self,
        identity: &str,
        run: &str,
        details: ActiveReflectionRunDetails,
        api: &maho_ext_api::ExtensionApi,
        now: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) -> Result<(), String> {
        lock(&self.active_runs).start(identity, run, details);
        self.sync_footer(now, ui);
        self.sync_rpc(api, now)
    }
    pub fn on_live_reflection_completed(
        &mut self,
        identity: &str,
        run: &str,
        api: &maho_ext_api::ExtensionApi,
        now: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) -> Result<(), String> {
        lock(&self.active_runs).settle(identity, run);
        self.sync_footer(now, ui);
        self.sync_rpc(api, now)
    }
    fn sync_footer(&mut self, now: i64, ui: Option<&mut dyn MemoryFooterUi>) {
        let session = self.session.as_deref();
        let context = session.and_then(|session| self.contexts.get(session));
        let active =
            context.is_some_and(|context| lock(&self.active_runs).is_active(&context.identity));
        self.footer.sync_active(context, session, active, now, ui);
    }
    pub fn on_settled(
        &mut self,
        session: &str,
        api: &maho_ext_api::ExtensionApi,
        now: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) -> Result<(), String> {
        let context = self.contexts.get(session);
        let active =
            context.is_some_and(|context| lock(&self.active_runs).is_active(&context.identity));
        self.footer.refresh(context, Some(session), active, now, ui);
        self.sync_rpc(api, now)
    }
    pub fn sync_rpc(&mut self, api: &maho_ext_api::ExtensionApi, now: i64) -> Result<(), String> {
        let runs = lock(&self.active_runs);
        let mut rpc = lock(&self.rpc);
        rpc.sync_native(
            &Deps {
                contexts: &self.contexts,
                runs: &runs,
            },
            api,
            now,
        )
    }
    pub fn shutdown(&mut self, identity: Option<&str>) {
        if let Some(identity) = identity {
            lock(&self.active_runs).clear(identity);
        }
        self.footer.dispose();
        lock(&self.rpc).detach();
    }
    pub fn unbind(&mut self, session: &str) {
        self.contexts.remove(session);
    }
    pub fn clear_status(&mut self) {
        self.footer.stop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Live {
        initial: usize,
        entries: Vec<String>,
    }
    impl crate::worker::completion_delivery::ReflectionLiveSession for Live {
        fn session_id(&self) -> &str {
            "session"
        }
        fn append_entry(&mut self, kind: &str, _: serde_json::Value) {
            self.entries.push(kind.into());
        }
        fn notify(&mut self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn warn(&mut self, message: &str, error: &str) {
            panic!("{message}: {error}")
        }
        fn on_completion(&mut self, _: &str) {}
    }
    impl crate::worker::health_alert::ReflectionHealthLiveSession for Live {
        fn session_id(&self) -> &str {
            "session"
        }
        fn has_ui(&self) -> bool {
            true
        }
        fn append_entry(
            &mut self,
            name: &str,
            _: &crate::worker::health_alert::ReflectionHealthEntry,
        ) {
            self.entries.push(name.into());
        }
        fn notify(&mut self, _: &str, _: &str) {}
    }
    impl ReflectionBindLive for Live {
        fn refresh_initial_status(&mut self, _: &MemoryIdentityContext) {
            self.initial += 1;
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
    #[test]
    fn bind_refreshes_initial_status_and_drains_pending_completions() {
        use crate::worker::completion_contracts::{
            CompletionDelivery, DeliveryStatus, REFLECTION_COMPLETION_ENTRY_TYPE,
            ReflectionCompletionRecord,
        };
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        crate::engine_session::prepare_memory_engine_session("agent", &paths, Default::default())
            .unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let finished = "2026-08-16T12:00:00.000Z";
        let now = chrono::DateTime::parse_from_rfc3339(finished)
            .unwrap()
            .timestamp_millis();
        crate::worker::completion_records::write_completion_record(
            &paths.reflection.join("completions"),
            &ReflectionCompletionRecord {
                schema_version: 1,
                run_id: "run-1".into(),
                identity: "agent".into(),
                category: "quick".into(),
                model: None,
                thinking: None,
                conversation_ids: vec![],
                trigger: memory_core::reflection::machine::ReflectionTrigger::Manual,
                origin: None,
                outcome: "merged".into(),
                reason: None,
                detail: None,
                started_at: finished.into(),
                finished_at: finished.into(),
                duration_ms: None,
                merged_commit_sha: None,
                files_changed: None,
                consecutive_failures: None,
                delivery: CompletionDelivery {
                    status: DeliveryStatus::Pending,
                    session_id: None,
                    consumed_at: None,
                },
            },
        )
        .unwrap();
        let api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let mut wiring = MemoryReflectionLiveWiring::default();
        wiring.active_runs.lock().unwrap().start(
            "agent",
            "run-1",
            ActiveReflectionRunDetails {
                trigger: "manual".into(),
                category: "quick".into(),
                model: None,
                started_at: finished.into(),
            },
        );
        let mut live = Live::default();
        wiring.attach("session");
        wiring
            .bind("session", context, &api, now, None, Some(&mut live))
            .unwrap();
        assert_eq!(live.initial, 1);
        assert_eq!(live.entries, [REFLECTION_COMPLETION_ENTRY_TYPE]);
        assert!(!wiring.active_runs.lock().unwrap().is_active("agent"));
    }
    #[tokio::test]
    async fn registered_status_pull_reports_bound_session_and_active_run() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        crate::engine_session::prepare_memory_engine_session("agent", &paths, Default::default())
            .unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let mut wiring = MemoryReflectionLiveWiring::default();
        wiring
            .register_rpc(
                &mut api,
                Arc::new(move || BTreeMap::from([("session".to_owned(), context.clone())])),
                Arc::new(|| 0),
                None,
            )
            .unwrap();
        let handler =
            api.registered.rpc_handlers[crate::memory_rpc_bridge::MEMORY_STATUS_RPC_METHOD].clone();
        assert_eq!(
            handler(serde_json::Value::Null).await.unwrap()["kind"],
            "unavailable"
        );
        wiring.attach("session");
        wiring.active_runs.lock().unwrap().start(
            "agent",
            "run",
            ActiveReflectionRunDetails {
                trigger: "manual".into(),
                category: "quick".into(),
                model: None,
                started_at: "now".into(),
            },
        );
        let snapshot = handler(serde_json::Value::Null).await.unwrap();
        assert_eq!(snapshot["journal"]["sessionId"], "session");
        assert_eq!(snapshot["reflection"]["activeRun"]["runId"], "run");
    }
    #[derive(Default)]
    struct Ui(Vec<String>);
    impl MemoryFooterUi for Ui {
        fn set_status(&mut self, _: &str, text: &str) {
            self.0.push(text.into());
        }
    }
    #[test]
    fn native_launch_completion_and_shutdown_publish_rpc_and_restore_footer() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let now = engine.repo.head_commit_timestamp().unwrap().unwrap() * 1000;
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let captured = events.clone();
        let _subscription = api.events.on(
            "senpi:extension-rpc-event",
            std::sync::Arc::new(move |value| match captured.lock() {
                Ok(mut events) => events.push(value.clone()),
                Err(error) => panic!("capture lock poisoned: {error}"),
            }),
        );
        let mut wiring = MemoryReflectionLiveWiring::default();
        let mut ui = Ui::default();
        wiring.attach("session");
        wiring
            .bind("session", context, &api, now, Some(&mut ui), None)
            .unwrap();
        wiring
            .on_reflection_launched(
                "agent",
                "run",
                ActiveReflectionRunDetails {
                    trigger: "manual".into(),
                    category: "quick".into(),
                    model: Some("p/m".into()),
                    started_at: "now".into(),
                },
                &api,
                now,
                Some(&mut ui),
            )
            .unwrap();
        assert!(ui.0.last().unwrap().ends_with("reflecting"));
        wiring
            .on_live_reflection_completed("agent", "run", &api, now, Some(&mut ui))
            .unwrap();
        assert_eq!(ui.0.last().unwrap(), "mem:agent just now");
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1]["data"]["reflection"]["activeRun"]["runId"], "run");
        assert!(events[2]["data"]["reflection"].get("activeRun").is_none());
        drop(events);
        wiring.shutdown(Some("agent"));
        assert!(!wiring.active_runs.lock().unwrap().is_active("agent"));
        let before = ui.0.len();
        wiring.footer.tick(&mut ui);
        assert_eq!(before, ui.0.len());
    }
    #[test]
    fn session_attached_after_shutdown_animates_footer_again() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let now = engine.repo.head_commit_timestamp().unwrap().unwrap() * 1000;
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let mut wiring = MemoryReflectionLiveWiring::default();
        let mut ui = Ui::default();
        wiring.attach("one");
        wiring
            .bind("one", context.clone(), &api, now, Some(&mut ui), None)
            .unwrap();
        wiring.shutdown(Some("agent"));
        assert!(wiring.footer.is_disposed());
        wiring.attach("two");
        wiring
            .bind("two", context, &api, now, Some(&mut ui), None)
            .unwrap();
        wiring
            .on_reflection_launched(
                "agent",
                "run",
                ActiveReflectionRunDetails {
                    trigger: "manual".into(),
                    category: "quick".into(),
                    model: None,
                    started_at: "now".into(),
                },
                &api,
                now,
                Some(&mut ui),
            )
            .unwrap();
        assert!(wiring.footer.is_animating());
        assert_eq!(ui.0.last().unwrap(), "mem:agent ⠋ reflecting");
        wiring.footer.tick(&mut ui);
        assert_eq!(ui.0.last().unwrap(), "mem:agent ⠙ reflecting");
    }
}
