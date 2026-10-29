type ArcSyncStatus = std::sync::Arc<
    dyn Fn(&std::path::Path) -> Option<crate::sync_runner::SyncRepoStatus> + Send + Sync,
>;
use crate::{context::MemoryIdentityContext, status_active_runs::ActiveReflectionRun};
use memory_core::git::{GitMemoryRepo, GitMemoryRepoOptions, errors::GitError};
use std::collections::BTreeMap;
pub const MEMORY_UPDATED_RPC_EVENT: &str = "omo.memory.updated";
pub const MEMORY_STATUS_RPC_METHOD: &str = "omo.memory.status";
pub use crate::facts_apply_rpc::MEMORY_FACTS_APPLY_RPC_METHOD;
pub trait MemoryRpcBridgeDeps {
    fn resolve_context(&self, session: &str) -> Option<&MemoryIdentityContext>;
    fn active_run(&self, identity: &str) -> Option<&ActiveReflectionRun>;
}
pub trait MemoryRpcEmitter {
    fn emit(&mut self, name: &str, data: &serde_json::Value);
}
pub type MemoryRpcDepsResolver =
    std::sync::Arc<dyn Fn() -> Box<dyn MemoryRpcBridgeDeps + Send> + Send + Sync>;
pub fn register_memory_rpc_bridge(
    api: &mut maho_ext_api::ExtensionApi,
    bridge: std::sync::Arc<std::sync::Mutex<MemoryRpcBridge>>,
    deps: MemoryRpcDepsResolver,
    now: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
    apply_facts: Option<crate::facts_apply_rpc::MemoryFactsApplier>,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let status_bridge = bridge.clone();
    let status_deps = deps.clone();
    let status_now = now.clone();
    api.rpc_handle(
        MEMORY_STATUS_RPC_METHOD,
        std::sync::Arc::new(move |_| {
            // Resolve deps before taking the bridge lock: sync paths lock runs then bridge.
            let deps = status_deps();
            let now = status_now();
            let result = status_bridge
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .status(deps.as_ref(), now);
            Box::pin(async move {
                result.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
            })
        }),
    )?;
    // `omo.memory.facts.apply`: the facts extractor's apply for the bound session; absent registers nothing.
    let Some(apply) = apply_facts else {
        return Ok(());
    };
    let publisher = std::sync::Arc::new(maho_ext_api::ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    api.rpc_handle(MEMORY_FACTS_APPLY_RPC_METHOD,std::sync::Arc::new(move |request|{
        let session=bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner).bound_session();
        let apply=apply.clone();let bridge=bridge.clone();let deps=deps.clone();let now=now.clone();let publisher=publisher.clone();
        Box::pin(async move{
            let Some(session)=session else{return Ok(serde_json::json!({"kind":"unavailable","reason":"No bound memory session."}));};
            let applied=apply(request,Some(session)).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            if applied["outcome"]=="committed"{
                let deps=deps();let now=now();
                // A failed push never fails the apply that already committed.
                let _=bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sync_native(deps.as_ref(),&publisher,now);
            }
            Ok(applied)
        })
    }))
}
#[derive(Default)]
pub struct MemoryRpcBridge {
    pub sync_status: Option<ArcSyncStatus>,
    session_id: Option<String>,
    last_snapshot: Option<String>,
    disposed: bool,
    repos: BTreeMap<std::path::PathBuf, GitMemoryRepo>,
    token_estimates: BTreeMap<String, usize>,
}
impl MemoryRpcBridge {
    pub fn attach(&mut self, session: &str) {
        if self.disposed {
            return;
        }
        self.session_id = Some(session.into());
        self.last_snapshot = None;
    }
    pub fn bound_session(&self) -> Option<String> {
        if self.disposed {
            None
        } else {
            self.session_id.clone()
        }
    }
    pub fn detach(&mut self) {
        self.session_id = None;
        self.last_snapshot = None;
    }
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.detach();
    }
    fn build_snapshot(
        &mut self,
        deps: &dyn MemoryRpcBridgeDeps,
        now: i64,
    ) -> Result<Option<serde_json::Value>, GitError> {
        if self.disposed {
            return Ok(None);
        }
        let Some(session) = &self.session_id else {
            return Ok(None);
        };
        let Some(context) = deps.resolve_context(session) else {
            return Ok(None);
        };
        let repo = match self.repos.entry(context.identity_paths.repo.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(GitMemoryRepo::new(
                GitMemoryRepoOptions::new(&context.identity_paths.repo, "omo-memory-rpc"),
            )?),
        };
        let repo_state = crate::memory_rpc_snapshot_state::read_memory_rpc_repo_state(
            repo,
            &mut self.token_estimates,
        )?;
        let journal =
            crate::memory_rpc_snapshot_state::read_memory_rpc_journal_state(context, session);
        let health = crate::memory_rpc_snapshot_state::read_memory_rpc_health(context, now);
        let mut reflection = serde_json::json!({"backlogSteps":journal.backlog_steps,"pendingCompaction":journal.pending_compaction,"consecutiveFailures":health["streak"]});
        if let Some(run) = deps.active_run(&context.identity) {
            let mut value = serde_json::json!({"runId":run.run_id,"trigger":run.details.trigger,"category":run.details.category,"startedAt":run.details.started_at});
            if let Some(model) = &run.details.model {
                value["model"] = model.clone().into();
            }
            reflection["activeRun"] = value;
        }
        if let Some(last) = health.get("lastOutcome") {
            reflection["lastOutcome"] = last.clone();
        }
        if let Some(fingerprint) = health.get("fingerprint") {
            reflection["lastFailureFingerprint"] = fingerprint.clone();
        }
        let mut snapshot = serde_json::json!({"schemaVersion":1,"identity":context.identity,"repo":repo_state,"reflection":reflection,"journal":{"sessionId":session,"totalSteps":journal.total_steps,"reflectedSteps":journal.reflected_steps}});
        if let Some(status) = self
            .sync_status
            .as_ref()
            .and_then(|read| read(&context.identity_paths.repo))
        {
            snapshot["sync"] = crate::sync_status_cached::rpc_status(&status);
        }
        Ok(Some(snapshot))
    }
    pub fn sync(
        &mut self,
        deps: &dyn MemoryRpcBridgeDeps,
        emitter: Option<&mut dyn MemoryRpcEmitter>,
        now: i64,
    ) -> Result<(), GitError> {
        let Some(emitter) = emitter else {
            return Ok(());
        };
        let Some(snapshot) = self.build_snapshot(deps, now)? else {
            return Ok(());
        };
        let fingerprint = snapshot.to_string();
        if self.last_snapshot.as_ref() == Some(&fingerprint) {
            return Ok(());
        }
        self.last_snapshot = Some(fingerprint);
        emitter.emit(MEMORY_UPDATED_RPC_EVENT, &snapshot);
        Ok(())
    }
    pub fn status(
        &mut self,
        deps: &dyn MemoryRpcBridgeDeps,
        now: i64,
    ) -> Result<serde_json::Value, GitError> {
        Ok(self.build_snapshot(deps, now)?.unwrap_or_else(
            || serde_json::json!({"kind":"unavailable","reason":"No bound memory session."}),
        ))
    }
    pub fn sync_native(
        &mut self,
        deps: &dyn MemoryRpcBridgeDeps,
        api: &maho_ext_api::ExtensionApi,
        now: i64,
    ) -> Result<(), String> {
        let Some(snapshot) = self
            .build_snapshot(deps, now)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let fingerprint = snapshot.to_string();
        if self.last_snapshot.as_ref() == Some(&fingerprint) {
            return Ok(());
        }
        self.last_snapshot = Some(fingerprint);
        api.rpc_emit(MEMORY_UPDATED_RPC_EVENT, &snapshot)
            .map_err(|error| error.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Deps {
        context: Option<MemoryIdentityContext>,
    }
    impl MemoryRpcBridgeDeps for Deps {
        fn resolve_context(&self, _: &str) -> Option<&MemoryIdentityContext> {
            self.context.as_ref()
        }
        fn active_run(&self, _: &str) -> Option<&ActiveReflectionRun> {
            None
        }
    }
    #[derive(Default)]
    struct Emitter(Vec<serde_json::Value>);
    impl MemoryRpcEmitter for Emitter {
        fn emit(&mut self, name: &str, data: &serde_json::Value) {
            assert_eq!(name, MEMORY_UPDATED_RPC_EVENT);
            self.0.push(data.clone());
        }
    }
    fn deps(root: &std::path::Path) -> Deps {
        let paths = memory_core::identity::layout::build_identity_paths(root, "agent");
        crate::engine_session::prepare_memory_engine_session("agent", &paths, Default::default())
            .unwrap();
        Deps {
            context: Some(MemoryIdentityContext::new(
                "agent".into(),
                paths,
                crate::binding::MemorySessionBinding {
                    identity: "agent".into(),
                    repo_path_hash: "hash".into(),
                    bound_at: 0.0,
                },
            )),
        }
    }
    #[test]
    fn attach_resets_snapshot_fingerprint() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        let mut emitter = Emitter::default();
        bridge.attach("one");
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        bridge.attach("two");
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0.len(), 2);
        assert_eq!(emitter.0[1]["journal"]["sessionId"], "two");
    }
    #[test]
    fn detached_pull_is_unavailable_without_reading_repository() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        assert_eq!(bridge.status(&deps, 0).unwrap()["kind"], "unavailable");
        assert!(bridge.repos.is_empty());
    }
    #[test]
    fn missing_rpc_never_builds_bound_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        bridge.attach("session");
        bridge.sync(&deps, None, 0).unwrap();
        assert!(bridge.repos.is_empty());
        assert!(bridge.token_estimates.is_empty());
        assert!(bridge.last_snapshot.is_none());
    }
    #[test]
    fn pull_does_not_consume_next_push() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        bridge.attach("session");
        let snapshot = bridge.status(&deps, 0).unwrap();
        assert!(bridge.last_snapshot.is_none());
        let mut emitter = Emitter::default();
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0, [snapshot]);
    }
    #[test]
    fn dirty_repository_changes_push_fingerprint() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        bridge.attach("session");
        let mut emitter = Emitter::default();
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        std::fs::write(
            deps.context
                .as_ref()
                .unwrap()
                .identity_paths
                .repo
                .join("new.md"),
            "uncommitted",
        )
        .unwrap();
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0.len(), 2);
        assert_eq!(emitter.0[1]["repo"]["dirty"], true);
    }
    #[tokio::test]
    async fn native_registered_status_handler_matches_rpc_wire_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let context = deps(root.path()).context.unwrap();
        let bridge = std::sync::Arc::new(std::sync::Mutex::new(MemoryRpcBridge::default()));
        bridge.lock().unwrap().attach("session");
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", root.path().into(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        register_memory_rpc_bridge(
            &mut api,
            bridge.clone(),
            std::sync::Arc::new(move || {
                Box::new(Deps {
                    context: Some(context.clone()),
                })
            }),
            std::sync::Arc::new(|| 0),
            None,
        )
        .unwrap();
        let snapshot =
            (api.registered.rpc_handlers[MEMORY_STATUS_RPC_METHOD])(serde_json::Value::Null)
                .await
                .unwrap();
        assert_eq!(snapshot["identity"], "agent");
        assert_eq!(snapshot["journal"]["sessionId"], "session");
        let event = serde_json::json!({"type":"extension_event","name":MEMORY_UPDATED_RPC_EVENT,"data":snapshot});
        let wire = maho_rpc::jsonl::serialize_json_line(
            maho_rpc::json_event::to_json_event(&event)
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&wire).unwrap(),
            event
        );
        bridge.lock().unwrap().detach();
        assert_eq!(
            (api.registered.rpc_handlers[MEMORY_STATUS_RPC_METHOD])(serde_json::Value::Null)
                .await
                .unwrap()["kind"],
            "unavailable"
        );
    }
    #[test]
    fn native_rpc_push_uses_host_bus_and_dedupes() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        bridge.attach("session");
        let api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", root.path().into(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = events.clone();
        let _subscription = api.events.on(
            "senpi:extension-rpc-event",
            std::sync::Arc::new(move |event| match capture.lock() {
                Ok(mut events) => events.push(event.clone()),
                Err(error) => panic!("capture lock poisoned: {error}"),
            }),
        );
        bridge.sync_native(&deps, &api, 0).unwrap();
        bridge.sync_native(&deps, &api, 0).unwrap();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["name"], MEMORY_UPDATED_RPC_EVENT);
        assert_eq!(events[0]["data"]["identity"], "agent");
    }
    #[test]
    fn real_snapshot_dedupes_and_pull_does_not_emit() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        let mut emitter = Emitter::default();
        bridge.attach("session");
        for _ in 0..3 {
            bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        }
        assert_eq!(emitter.0.len(), 1);
        let snapshot = bridge.status(&deps, 0).unwrap();
        assert_eq!(snapshot, emitter.0[0]);
        assert_eq!(snapshot["identity"], "agent");
        assert_eq!(snapshot["journal"]["sessionId"], "session");
        assert_eq!(emitter.0.len(), 1);
    }
    #[test]
    fn no_rpc_does_no_git_and_unbound_is_unavailable() {
        let deps = Deps { context: None };
        let mut bridge = MemoryRpcBridge::default();
        assert_eq!(bridge.status(&deps, 0).unwrap()["kind"], "unavailable");
        bridge.attach("session");
        bridge.sync(&deps, None, 0).unwrap();
        assert!(bridge.repos.is_empty());
        let mut emitter = Emitter::default();
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert!(emitter.0.is_empty());
        assert!(bridge.repos.is_empty());
    }
    #[test]
    fn detach_reattach_and_dispose() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        let mut emitter = Emitter::default();
        bridge.attach("session");
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        bridge.detach();
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0.len(), 1);
        bridge.attach("session");
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0.len(), 2);
        bridge.dispose();
        bridge.attach("session");
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0.len(), 2);
        assert_eq!(bridge.status(&deps, 0).unwrap()["kind"], "unavailable");
    }
    #[test]
    fn journal_change_pushes_fresh_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let deps = deps(root.path());
        let mut bridge = MemoryRpcBridge::default();
        let mut emitter = Emitter::default();
        bridge.attach("session");
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        let path = deps
            .context
            .as_ref()
            .unwrap()
            .identity_paths
            .transcripts
            .join("session");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("state.json"),r#"{"total_completed_steps":42,"reflected_completed_steps":28,"steps_since_last_successful_reflection":14,"pending_compaction":true}"#).unwrap();
        bridge.sync(&deps, Some(&mut emitter), 0).unwrap();
        assert_eq!(emitter.0.len(), 2);
        assert_eq!(emitter.0[1]["reflection"]["backlogSteps"], 14.0);
        assert_eq!(emitter.0[1]["journal"]["totalSteps"], 42.0);
    }
}
