//! Identity context; reading it never creates storage.
use std::path::Path;
use memory_core::identity::layout::MemoryIdentityPaths;
use crate::binding::MemorySessionBinding;

#[derive(Clone, Debug, Default)]
pub struct MemoryPendingLedger {
    pub pending_compaction: bool,
    pub config_restart_notified: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectScope {
    pub memory_root: std::path::PathBuf,
    pub auto_identity: String,
}

#[derive(Clone, Debug)]
pub struct MemoryIdentityContext {
    pub identity: String,
    pub identity_paths: MemoryIdentityPaths,
    pub durable_root: std::path::PathBuf,
    pub project_scope: Option<ProjectScope>,
    pub binding: MemorySessionBinding,
    pub ledger: std::sync::Arc<std::sync::Mutex<MemoryPendingLedger>>,
}

impl MemoryIdentityContext {
    /// Pin `context.ts:34-36`: every bind gets one fresh ledger object that the trigger wiring and
    /// the RPC snapshot share; clones of this context share it too.
    pub fn new(identity: String, identity_paths: MemoryIdentityPaths, binding: MemorySessionBinding) -> Self {
        let durable_root=identity_paths.root.clone();
        Self { identity, identity_paths, durable_root, project_scope: None, binding, ledger: std::sync::Arc::new(std::sync::Mutex::new(MemoryPendingLedger::default())) }
    }

    pub fn with_durable_root(mut self, durable_root: std::path::PathBuf) -> Self { self.durable_root=durable_root; self }

    pub fn with_project_scope(mut self, scope: ProjectScope) -> Self { self.project_scope = Some(scope); self }

    pub fn repo_path(&self) -> &Path { &self.identity_paths.repo }

    pub fn ensure_runtime_dirs(&self) -> std::io::Result<()> {
        ensure_identity_runtime_dirs(&self.identity_paths)
    }
}

pub fn ensure_identity_runtime_dirs(paths: &MemoryIdentityPaths) -> std::io::Result<()> {
    for path in [&paths.locks, &paths.transcripts, &paths.reflection, &paths.reflection_sessions, &paths.worktrees, &paths.viewers, &paths.push_queue, &paths.facts_queue, &paths.facts, &paths.notices, &paths.tool_receipts] {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::identity::layout::build_identity_paths;
    #[test]
    fn reading_context_creates_no_storage() {
        let root = tempfile::tempdir().unwrap();
        let paths = build_identity_paths(root.path(), "agent-123");
        let context = MemoryIdentityContext::new("agent-123".into(), paths.clone(), MemorySessionBinding { identity: "agent-123".into(), repo_path_hash: "hash".into(), bound_at: 1.0 });
        assert_eq!(context.repo_path(), paths.repo);
        assert!(!paths.repo.exists());
        assert!(!paths.runtime.exists());
    }
    #[test]
    fn first_write_ensures_only_runtime_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let paths = build_identity_paths(root.path(), "agent-123");
        ensure_identity_runtime_dirs(&paths).unwrap();
        ensure_identity_runtime_dirs(&paths).unwrap();
        for path in [&paths.runtime, &paths.locks, &paths.transcripts, &paths.reflection, &paths.reflection_sessions, &paths.worktrees, &paths.viewers, &paths.push_queue, &paths.facts_queue, &paths.facts, &paths.notices, &paths.tool_receipts] { assert!(path.is_dir()); }
        assert!(!paths.repo.exists());
    }
}
