//! Session binding for the memory component (pin `session-binding.ts`): the host's session surface,
//! the per-session state, and the bind that resolves the session's identity, records its binding,
//! and acquires the module supervisor and the sync runner for durable sessions.
use std::{
    collections::BTreeMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

use maho_ext_api::{ExtensionContext, ExtensionUi, NotificationType};
use memory_core::identity::{
    layout::{MemoryIdentityPaths, resolve_memory_root},
    resolve::resolve_memory_identity,
};

use crate::binding::{
    MEMORY_BINDING_CUSTOM_TYPE, MemorySessionBinding, SessionEntryLike, create_memory_binding,
    find_latest_memory_binding,
};
use crate::context::MemoryIdentityContext;
use crate::identity_adoption::{BoundIdentity, IdentityAdoption, adopt_session_identity};
use crate::stale_extension_context::is_stale_extension_context_error;
use crate::supervisor::MEMORY_MODULE_SUPERVISOR;
use crate::transient_identity::{IdentityRunPaths, resolve_identity_run_paths};

pub const UNKNOWN_SESSION_ID: &str = "unknown-session";

/// The session's full history is read only when a bind needs its recorded binding (senpi#3110).
pub type ReadEntries = Arc<dyn Fn() -> Vec<SessionEntryLike> + Send + Sync>;

#[derive(Clone)]
pub struct SessionSurface {
    pub read_entries: ReadEntries,
    pub id: String,
    pub ui: Option<Arc<dyn ExtensionUi>>,
    pub has_ui: Option<bool>,
    /// The session's own working directory, reported by the host per event.
    pub cwd: Option<PathBuf>,
}

pub struct SessionState {
    pub enabled: bool,
    pub ui: Option<Arc<dyn ExtensionUi>>,
    pub context: Option<MemoryIdentityContext>,
    /// Where this session's identity storage lives; finalized (transient root reclaimed) at shutdown.
    pub run: Option<IdentityRunPaths>,
    pub memory_status_attempted: bool,
    /// The repository this session holds the sync runner for; released with the binding.
    pub sync_repo: Option<PathBuf>,
    pub restart_notified: bool,
    pub conflict_notified: bool,
}

impl SessionState {
    pub fn new(enabled: bool, ui: Option<Arc<dyn ExtensionUi>>) -> Self {
        Self {
            enabled,
            ui,
            context: None,
            run: None,
            memory_status_attempted: false,
            sync_repo: None,
            restart_notified: false,
            conflict_notified: false,
        }
    }
}

/// The end-to-end encrypted sync runner a durable session holds (pin `SyncRunner.acquire/release`).
pub trait SessionSyncRunner: Send + Sync {
    fn acquire(&self, identity: &str, paths: &MemoryIdentityPaths, sync: &serde_json::Value);
    fn release(&self, repo: &std::path::Path);
}

pub type SessionAfterBind = Arc<
    dyn Fn(
            String,
            MemoryIdentityContext,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;
/// `(level, message)`; level is `info` or `warn`.
pub type LoadMemorySettings =
    Arc<dyn Fn(&std::path::Path) -> Result<serde_json::Value, String> + Send + Sync>;
pub type SessionLogger = Arc<dyn Fn(&str, &str) + Send + Sync>;
pub type SessionAppendEntry = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub struct SessionBinderDeps {
    pub append_entry: SessionAppendEntry,
    pub logger: SessionLogger,
    pub env: BTreeMap<String, String>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub cwd: PathBuf,
    /// The resolved memory settings for `cwd` (`agent`, `sync`, ...).
    pub load_settings: LoadMemorySettings,
    pub sessions: Arc<Mutex<BTreeMap<String, SessionState>>>,
    pub after_bind: SessionAfterBind,
    pub sync_runner: Arc<dyn SessionSyncRunner>,
    pub is_enabled: Arc<dyn Fn(&serde_json::Value) -> bool + Send + Sync>,
    /// Pin `isOneShotSurface`: a session role marks one-shot, else a UI-less surface does.
    pub is_one_shot: Arc<dyn Fn(&SessionSurface) -> bool + Send + Sync>,
}

pub struct BindOptions {
    pub existing: Option<SessionState>,
    pub verify_repository: bool,
}

pub type SessionBinder =
    Box<dyn Fn(&SessionSurface, BindOptions) -> Result<(), String> + Send + Sync>;

pub fn create_session_binder(deps: SessionBinderDeps) -> SessionBinder {
    Box::new(move |surface: &SessionSurface, options: BindOptions| {
        let settings = (deps.load_settings)(&deps.cwd)?;
        let mut state = options
            .existing
            .unwrap_or_else(|| SessionState::new((deps.is_enabled)(&settings), surface.ui.clone()));
        if !state.enabled {
            deps.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(surface.id.clone(), state);
            return Ok(());
        }
        // The identity belongs to the SESSION's workspace, not the host process's directory (#8556).
        let session_cwd = surface.cwd.clone().unwrap_or_else(|| deps.cwd.clone());
        let memory_root = resolve_memory_root(&deps.env, &session_cwd);
        let agent = settings["agent"].as_str();
        let resolved = if memory_core::identity::is_auto_agent_value(agent) {
            memory_core::identity::resolve_auto_identity(
                &session_cwd,
                &deps.env,
                if settings["auto_identity"] == "git" {
                    memory_core::identity::AutoIdentityMode::Git
                } else {
                    memory_core::identity::AutoIdentityMode::Path
                },
            )
        } else {
            resolve_memory_identity(agent, &session_cwd, &deps.env)
        }
        .map_err(|error| error.to_string())?;
        let entries = (surface.read_entries)();
        let previous = find_latest_memory_binding(&entries).cloned();
        let adoption = adopt_session_identity(
            previous.as_ref(),
            BoundIdentity {
                id: resolved.id.clone(),
                paths: resolved.paths.clone(),
            },
            &memory_root,
            agent,
            options.verify_repository,
        );
        let (identity, rebound) = match adoption {
            IdentityAdoption::Conflict => {
                if !state.conflict_notified {
                    state.conflict_notified = true;
                    if let Some(ui) = &surface.ui {
                        ui.notify(&format!("memory identity conflict: session is bound to {}, but config resolved {}; restart with the original identity or fork a new session", previous.as_ref().map_or("", |binding| binding.identity.as_str()), resolved.id), NotificationType::Error);
                    }
                    (deps.logger)(
                        "warn",
                        &format!(
                            "omo-senpi memory binding failed closed: session {} bound {:?} resolved {}",
                            surface.id,
                            previous.map(|binding| binding.identity),
                            resolved.id
                        ),
                    );
                }
                deps.sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(surface.id.clone(), state);
                return Ok(());
            }
            IdentityAdoption::Resolved(identity) => (identity, false),
            IdentityAdoption::Rebound(identity) => (identity, true),
        };
        if rebound {
            (deps.logger)(
                "info",
                &format!(
                    "omo-senpi memory identity rebound to the session binding: session {} bound {} resolved {}",
                    surface.id, identity.id, resolved.id
                ),
            );
        }
        let binding = create_memory_binding(
            &identity.id,
            &identity.paths.repo.to_string_lossy(),
            (deps.now)(),
        );
        let one_shot = (deps.is_one_shot)(surface);
        let token = format!(
            "{:x}-{}-{}",
            ((deps.now)() as u64).max(1),
            std::process::id(),
            memory_core::support::random::random_uuid().replace('-', "")
        );
        let run = resolve_identity_run_paths(
            &identity.id,
            identity.paths.clone(),
            &memory_root,
            one_shot,
            Some(&token),
        );
        let context =
            MemoryIdentityContext::new(identity.id.clone(), run.paths.clone(), binding.clone())
                .with_durable_root(run.durable_root.clone())
                .with_project_scope(crate::context::ProjectScope { memory_root: memory_root.clone(), auto_identity: settings["auto_identity"].as_str().unwrap_or("project").to_owned() });
        state.run = Some(run);
        state.context = Some(context.clone());
        MEMORY_MODULE_SUPERVISOR.acquire();
        if !one_shot {
            state.sync_repo = Some(identity.paths.repo.clone());
            deps.sync_runner
                .acquire(&identity.id, &identity.paths, &settings["sync"]);
        }
        deps.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(surface.id.clone(), state);
        // Disposable bookkeeping: a session closed without a conversation leaves no file (senpi#3263).
        (deps.append_entry)(MEMORY_BINDING_CUSTOM_TYPE, binding_value(&binding));
        // Bind-time reconcile floats past the bind by design, but its rejection must not float.
        let work = (deps.after_bind)(surface.id.clone(), context);
        let logger = deps.logger.clone();
        tokio::spawn(async move {
            if let Err(error) = work.await {
                if is_stale_extension_context_error(&error) {
                    logger(
                        "info",
                        "memory bind-time reconcile skipped: session replaced before the bind completed",
                    );
                } else {
                    logger(
                        "warn",
                        &format!("memory bind-time reconcile failed: {error}"),
                    );
                }
            }
        });
        Ok(())
    })
}

fn binding_value(binding: &MemorySessionBinding) -> serde_json::Value {
    serde_json::json!({"identity": binding.identity, "repoPathHash": binding.repo_path_hash, "boundAt": binding.bound_at})
}

/// Releases a bound session's supervisor reference and sync-runner hold.
pub fn release_session(
    state: Option<&mut SessionState>,
    sync_runner: Option<&dyn SessionSyncRunner>,
) {
    let Some(state) = state else { return };
    if state.context.take().is_none() {
        return;
    }
    MEMORY_MODULE_SUPERVISOR.release();
    if let (Some(repo), Some(runner)) = (state.sync_repo.take(), sync_runner) {
        runner.release(&repo);
    }
}

/// Reads the host surface off a live ctx; never throws on a retired ctx (a stale guard error gives the unknown session).
pub fn read_session_surface(context: Option<&ExtensionContext>) -> SessionSurface {
    let Some(context) = context else {
        return SessionSurface {
            read_entries: Arc::new(Vec::new),
            id: UNKNOWN_SESSION_ID.into(),
            ui: None,
            has_ui: None,
            cwd: None,
        };
    };
    let id = context.session_manager.session_id();
    let manager = context.session_manager.clone();
    let read_entries: ReadEntries = Arc::new(move || {
        manager
            .get_entries()
            .iter()
            .filter_map(entry_like)
            .collect()
    });
    SessionSurface {
        read_entries,
        id: if id.is_empty() {
            UNKNOWN_SESSION_ID.into()
        } else {
            id.into()
        },
        ui: Some(context.ui.clone()),
        has_ui: Some(context.has_ui),
        cwd: (!context.cwd.as_os_str().is_empty()).then(|| context.cwd.clone()),
    }
}

fn entry_like(entry: &maho_ext_api::SessionEntry) -> Option<SessionEntryLike> {
    let data = &entry.data;
    let binding = (entry.kind == "custom" && data["customType"] == MEMORY_BINDING_CUSTOM_TYPE)
        .then(|| {
            Some(MemorySessionBinding {
                identity: data["data"]["identity"].as_str()?.into(),
                repo_path_hash: data["data"]["repoPathHash"].as_str()?.into(),
                bound_at: data["data"]["boundAt"].as_f64()?,
            })
        });
    Some(SessionEntryLike {
        entry_type: entry.kind.clone(),
        custom_type: data["customType"].as_str().map(str::to_owned),
        data: binding.flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_ctx_reads_as_the_unknown_session() {
        let surface = read_session_surface(None);
        assert_eq!(surface.id, UNKNOWN_SESSION_ID);
        assert!((surface.read_entries)().is_empty());
        assert!(surface.ui.is_none() && surface.has_ui.is_none() && surface.cwd.is_none());
    }

    #[test]
    fn release_drops_the_context_once_and_returns_the_sync_hold() {
        struct Runner(Mutex<Vec<PathBuf>>);
        impl SessionSyncRunner for Runner {
            fn acquire(&self, _: &str, _: &MemoryIdentityPaths, _: &serde_json::Value) {}
            fn release(&self, repo: &std::path::Path) {
                self.0.lock().unwrap().push(repo.to_path_buf());
            }
        }
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let mut state = SessionState::new(true, None);
        state.context = Some(MemoryIdentityContext::new(
            "agent".into(),
            paths,
            create_memory_binding("agent", "repo", 0.0),
        ));
        state.sync_repo = Some("/repo".into());
        MEMORY_MODULE_SUPERVISOR.acquire();
        let runner = Runner(Mutex::new(vec![]));
        release_session(Some(&mut state), Some(&runner));
        release_session(Some(&mut state), Some(&runner));
        assert!(state.context.is_none() && state.sync_repo.is_none());
        assert_eq!(runner.0.lock().unwrap().len(), 1);
    }
}
