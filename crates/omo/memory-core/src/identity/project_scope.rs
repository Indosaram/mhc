//! Existing project-memory ownership for touched files; never creates repositories.
use super::project_scope_fs::{EntryKind, ProjectScopeProbe};
use super::resolve::{AutoIdentityMode, resolve_auto_identity_with_probe};
use super::{MemoryIdentity, home_directory, resolve_memory_root};
use crate::support::paths::resolve_from;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};
pub const PROJECT_SCOPE_SCAN_LIMIT: usize = 1000;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub project_dir: PathBuf,
    pub identity: MemoryIdentity,
}
#[derive(Default)]
pub struct ProjectIdentityResolverOptions {
    pub session_cwd: PathBuf,
    pub memory_root: Option<PathBuf>,
    pub auto_identity: AutoIdentityMode,
    pub bound_id: Option<String>,
    pub env: BTreeMap<String, String>,
    pub fs: Option<Arc<ProjectScopeProbe>>,
}
struct ScopeRoots {
    session: PathBuf,
    home: PathBuf,
    memory: PathBuf,
}
pub struct ProjectIdentityResolver {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    mode: AutoIdentityMode,
    bound_id: Option<String>,
    probe: Arc<ProjectScopeProbe>,
    prepared: OnceCell<ScopeRoots>,
    links: Mutex<HashSet<PathBuf>>,
    checked: Mutex<HashSet<PathBuf>>,
    by_dir: Mutex<HashMap<PathBuf, Option<ProjectIdentity>>>,
}
pub fn create_project_identity_resolver(
    options: ProjectIdentityResolverOptions,
) -> ProjectIdentityResolver {
    let cwd = resolve_from(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        &options.session_cwd,
    );
    let mut env = options.env;
    let memory = options
        .memory_root
        .unwrap_or_else(|| resolve_memory_root(&env, &cwd));
    env.insert(
        super::MEMORY_ROOT_ENV_VAR.into(),
        memory.to_string_lossy().into_owned(),
    );
    ProjectIdentityResolver {
        cwd,
        env,
        mode: options.auto_identity,
        bound_id: options.bound_id,
        probe: options
            .fs
            .unwrap_or_else(|| Arc::new(ProjectScopeProbe::new())),
        prepared: OnceCell::new(),
        links: Mutex::new(HashSet::new()),
        checked: Mutex::new(HashSet::new()),
        by_dir: Mutex::new(HashMap::new()),
    }
}
impl ProjectIdentityResolver {
    async fn real_path_or_resolved(&self, path: &Path) -> PathBuf {
        let start = resolve_from(Path::new("."), path);
        let mut current = start.as_path();
        let mut missing = Vec::new();
        loop {
            if let Some(mut real) = self.probe.real_path(current).await {
                for name in missing.iter().rev() {
                    real.push(name);
                }
                return real;
            }
            let Some(parent) = current.parent() else {
                return start;
            };
            if let Some(name) = current.file_name() {
                missing.push(name.to_os_string());
            }
            current = parent;
        }
    }
    async fn is_inside_git_work_tree(&self, path: &Path) -> bool {
        for dir in path.ancestors() {
            if self.probe.exists(&dir.join(".git")).await {
                return true;
            }
        }
        false
    }
    async fn consider_link(&self, path: &Path, scope: &ScopeRoots) {
        let mut checked = self.checked.lock().await;
        if checked.contains(path) {
            return;
        }
        if self.probe.link_kind(path).await == Some(EntryKind::Symlink)
            && let Some(target) = self.probe.real_path(path).await
                && self.probe.link_kind(&target).await == Some(EntryKind::Directory)
                    && !self
                        .is_inside_git_work_tree(path.parent().unwrap_or(path))
                        .await
                    && !scope.session.starts_with(&target)
                    && !scope.home.starts_with(&target)
                    && !scope.memory.starts_with(&target)
                {
                    self.links.lock().await.insert(target);
                }
        checked.insert(path.to_path_buf());
    }
    async fn scan_session_tree(&self, scope: &ScopeRoots) {
        if self.is_inside_git_work_tree(&self.cwd).await {
            return;
        }
        let mut budget = PROJECT_SCOPE_SCAN_LIMIT;
        let mut queue = vec![(self.cwd.clone(), 1)];
        while let Some((dir, depth)) = queue.pop() {
            for name in self.probe.readdir(&dir).await.unwrap_or_default() {
                if budget == 0 {
                    return;
                }
                budget -= 1;
                let path = dir.join(name);
                let kind = self.probe.link_kind(&path).await;
                self.consider_link(&path, scope).await;
                if depth == 1 && kind == Some(EntryKind::Directory) {
                    queue.push((path, 2));
                }
            }
        }
    }
    async fn prepare_once(&self) -> &ScopeRoots {
        self.prepared
            .get_or_init(|| async {
                let scope = ScopeRoots {
                    session: self.real_path_or_resolved(&self.cwd).await,
                    home: self.real_path_or_resolved(&home_directory()).await,
                    memory: self
                        .real_path_or_resolved(&resolve_memory_root(&self.env, &self.cwd))
                        .await,
                };
                self.scan_session_tree(&scope).await;
                scope
            })
            .await
    }
    async fn canonical_dir(&self, path: &Path) -> PathBuf {
        let real = self.real_path_or_resolved(path).await;
        if self.mode != AutoIdentityMode::Git {
            return real;
        }
        self.probe.prepare_git_layout(&real).await;
        match super::git_layout::resolve_linked_worktree_source(&real, self.probe.as_ref()) {
            Some(source) => self.real_path_or_resolved(&source.hash_source).await,
            None => real,
        }
    }
    pub async fn for_path(&self, file_path: &Path) -> Option<ProjectIdentity> {
        let scope = self.prepare_once().await;
        let lexical = resolve_from(&self.cwd, file_path);
        let lexical_dir = lexical.parent()?;
        if let Ok(parts) = lexical_dir.strip_prefix(&self.cwd) {
            let mut current = self.cwd.clone();
            for part in parts.components() {
                current.push(part);
                self.consider_link(&current, scope).await;
            }
        }
        let dir = self.canonical_dir(lexical_dir).await;
        let mut owner = if dir.starts_with(&scope.session) {
            Some((scope.session.clone(), false))
        } else {
            None
        };
        for root in self.links.lock().await.iter() {
            if dir.starts_with(root)
                && owner
                    .as_ref()
                    .is_none_or(|(best, _)| root.components().count() > best.components().count())
            {
                owner = Some((root.clone(), true));
            }
        }
        let (root, candidate) = owner?;
        for current in dir.ancestors() {
            if current == root && !candidate {
                return None;
            }
            let mut by_dir = self.by_dir.lock().await;
            if !by_dir.contains_key(current) {
                if self.mode == AutoIdentityMode::Git {
                    self.probe.prepare_git_layout(current).await;
                }
                // Path-mode ids hash the raw (session-relative) path, like the session's own
                // identity; `current` is canonical, which differs under a symlinked cwd (macOS /var).
                let id_dir = match current.strip_prefix(&scope.session) {
                    Ok(rest) if self.mode == AutoIdentityMode::Path => self.cwd.join(rest),
                    _ => current.to_path_buf(),
                };
                let found = match resolve_auto_identity_with_probe(
                    &id_dir,
                    &self.env,
                    self.mode,
                    self.probe.as_ref(),
                ) {
                    Ok(identity)
                        if self.bound_id.as_ref() != Some(&identity.id)
                            && self.probe.exists(&identity.paths.repo).await =>
                    {
                        Some(ProjectIdentity {
                            project_dir: current.to_path_buf(),
                            identity,
                        })
                    }
                    _ => None,
                };
                by_dir.insert(current.to_path_buf(), found);
            }
            if let Some(found) = by_dir.get(current).cloned().flatten() {
                return Some(found);
            }
            if current == root {
                return None;
            }
        }
        None
    }
    pub async fn real_path(&self, path: &Path) -> Option<PathBuf> {
        self.probe.real_path(path).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn nearest_existing_child_and_missing_path_are_resolved_without_creation() {
        let t = tempfile::tempdir().unwrap();
        let session = t.path().join("workspace");
        let child = session.join("child");
        let memory = t.path().join("memory");
        std::fs::create_dir_all(&child).unwrap();
        let mut env = BTreeMap::new();
        env.insert(
            super::super::MEMORY_ROOT_ENV_VAR.into(),
            memory.to_string_lossy().into_owned(),
        );
        let identity =
            super::super::resolve_auto_identity(&child, &env, AutoIdentityMode::Path).unwrap();
        std::fs::create_dir_all(&identity.paths.repo).unwrap();
        let resolver = create_project_identity_resolver(ProjectIdentityResolverOptions {
            session_cwd: session,
            memory_root: Some(memory),
            ..Default::default()
        });
        assert_eq!(
            resolver
                .for_path(&child.join("missing/file.rs"))
                .await
                .unwrap()
                .identity
                .id,
            identity.id
        );
        assert!(
            resolver
                .for_path(&t.path().join("outside/file.rs"))
                .await
                .is_none()
        );
    }
}
