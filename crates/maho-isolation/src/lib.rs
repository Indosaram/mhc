//! Workspace isolation for delegated tasks (MIT; see `PROVENANCE.md` and
//! `docs/cleanroom/specs/isolation.md`).
//!
//! [`TaskIsolation`] implements `maho_task::isolation::WorkspaceIsolation`: `prepare` gives an
//! isolated child its own git worktree (or a copy outside git) mirroring the parent's live state;
//! `finish` captures what the child changed as a patch, applies it to the parent (or commits it
//! on `mhc/task/<id>` and cherry-picks it), or keeps it, and removes the isolation. Settings come
//! from `task.isolation` in the merged config of the task's directory.
pub mod backend;
pub mod lifecycle;
pub mod owner;
pub mod settings;
pub mod tree;

pub use backend::BackendKind;
pub use lifecycle::{BRANCH_PREFIX, MERGED_DIR, Prepared};
pub use settings::{BackendChoice, IsolationSettings, MergeMode, WORKTREE_DIR_ENV, base_dir};

use maho_task::isolation::{IsolatedWorkspace, WorkspaceIsolation};
use maho_task::runner::BoxFuture;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type SettingsSource = Arc<dyn Fn(&Path) -> IsolationSettings + Send + Sync>;

/// The isolation backend handed to `maho-task`.
#[derive(Clone)]
pub struct TaskIsolation {
    root: PathBuf,
    settings: SettingsSource,
    live: Arc<Mutex<HashMap<String, Prepared>>>,
}

impl std::fmt::Debug for TaskIsolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskIsolation")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `task.isolation` of the merged config (user + project files, environment) for `cwd`.
pub fn settings_for(cwd: &Path) -> IsolationSettings {
    let loaded = maho_config::load_native(cwd, &maho_config::process_env());
    IsolationSettings::from_view(Some(&loaded.view))
}

impl TaskIsolation {
    /// Isolations under `root` (see [`base_dir`]); settings read from the config of each task's cwd.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_settings(root, Arc::new(settings_for))
    }

    /// Isolations under `root` with settings from `settings`.
    pub fn with_settings(root: impl Into<PathBuf>, settings: SettingsSource) -> Self {
        Self {
            root: root.into(),
            settings,
            live: Arc::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Removes isolations left by processes that are gone (see [`owner::clear_stale`]).
    pub fn clear_stale(&self, include_retained: bool) -> Vec<PathBuf> {
        owner::clear_stale(&self.root, include_retained)
    }

    /// Synchronous [`WorkspaceIsolation::prepare`].
    ///
    /// # Errors
    /// See [`lifecycle::prepare`].
    pub fn prepare_blocking(&self, task_id: &str, cwd: &Path) -> Result<IsolatedWorkspace, String> {
        let settings = (self.settings)(cwd);
        let prepared = lifecycle::prepare(&self.root, task_id, cwd, &settings)?;
        let handle = prepared.base.to_string_lossy().into_owned();
        let workspace = IsolatedWorkspace {
            cwd: prepared.cwd.clone(),
            handle: Some(handle.clone()),
        };
        lock(&self.live).insert(handle, prepared);
        Ok(workspace)
    }

    /// Synchronous [`WorkspaceIsolation::finish`].
    ///
    /// # Errors
    /// The workspace was not prepared by this backend.
    pub fn finish_blocking(
        &self,
        workspace: &IsolatedWorkspace,
        succeeded: bool,
    ) -> Result<Option<String>, String> {
        let prepared = workspace
            .handle
            .as_ref()
            .and_then(|handle| lock(&self.live).remove(handle))
            .ok_or_else(|| format!("unknown isolated workspace {}", workspace.cwd.display()))?;
        Ok(Some(lifecycle::finish(&prepared, succeeded)))
    }
}

impl WorkspaceIsolation for TaskIsolation {
    fn name(&self) -> &str {
        "workspace"
    }

    fn prepare<'a>(
        &'a self,
        task_id: &'a str,
        cwd: &'a Path,
    ) -> BoxFuture<'a, Result<IsolatedWorkspace, String>> {
        let this = self.clone();
        let task_id = task_id.to_owned();
        let cwd = cwd.to_path_buf();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.prepare_blocking(&task_id, &cwd))
                .await
                .map_err(|error| format!("isolation setup panicked: {error}"))?
        })
    }

    fn finish<'a>(
        &'a self,
        workspace: &'a IsolatedWorkspace,
        succeeded: bool,
    ) -> BoxFuture<'a, Result<Option<String>, String>> {
        let this = self.clone();
        let workspace = workspace.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.finish_blocking(&workspace, succeeded))
                .await
                .map_err(|error| format!("isolation teardown panicked: {error}"))?
        })
    }
}
