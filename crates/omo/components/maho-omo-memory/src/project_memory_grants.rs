//! Project memory read grants (#10044).
//!
//! Read-only exceptions to the cross-identity memory denial. A bound identity may READ the
//! committed notes of a project identity the project-scope resolver found inside its session's
//! working tree, and only after that resolver returned it. Enumeration, writes, the repo
//! directory itself, its git metadata and reserved `system/` tree, and every other identity
//! stay denied.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::context::MemoryIdentityContext;
use crate::policy_guard::{canonicalize_from_nearest_existing, resolve_path};

static GRANTS: LazyLock<Mutex<BTreeMap<String, BTreeSet<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

const EXCLUDED_TOP_LEVEL: [&str; 2] = [".git", "system"];

pub fn grant_project_memory_read(context: &MemoryIdentityContext, project_repo: &Path) {
    let canonical = std::fs::canonicalize(project_repo)
        .or_else(|_| resolve_path(project_repo))
        .unwrap_or_else(|_| project_repo.to_path_buf());
    let mut grants = GRANTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    grants.entry(context.identity.clone()).or_default().insert(canonical);
}

pub fn revoke_project_memory_reads(context: &MemoryIdentityContext) {
    let mut grants = GRANTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    grants.remove(&context.identity);
}

pub fn is_granted_project_memory_read(context: &MemoryIdentityContext, target: &Path) -> bool {
    let grants = GRANTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(roots) = grants.get(&context.identity) else {
        return false;
    };
    if roots.is_empty() {
        return false;
    }
    let resolved_target = canonicalize_from_nearest_existing(target)
        .or_else(|| resolve_path(target).ok())
        .unwrap_or_else(|| target.to_path_buf());

    for root in roots {
        if let Ok(rel) = resolved_target.strip_prefix(root) {
            let rel_str = rel.to_string_lossy();
            if rel_str.is_empty() {
                continue;
            }
            if let Some(first) = rel.components().next() {
                let first_name = first.as_os_str().to_string_lossy().to_lowercase();
                if EXCLUDED_TOP_LEVEL.contains(&first_name.as_str()) {
                    continue;
                }
            }
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::identity::layout::build_identity_paths;
    use crate::binding::MemorySessionBinding;

    fn fixture_context(root: &Path, identity: &str) -> MemoryIdentityContext {
        let paths = build_identity_paths(root, identity);
        MemoryIdentityContext::new(
            identity.to_string(),
            paths,
            MemorySessionBinding {
                identity: identity.to_string(),
                repo_path_hash: "hash".to_string(),
                bound_at: 0.0,
            },
        )
    }

    #[test]
    fn given_granted_project_repo_when_note_is_read_then_allowed() {
        let root = tempfile::tempdir().unwrap();
        let own = fixture_context(root.path(), "agent-own");
        let foreign_repo = root.path().join("foreign").join("repo");
        let foreign_note = foreign_repo.join("reference").join("deploy.md");
        std::fs::create_dir_all(foreign_note.parent().unwrap()).unwrap();
        std::fs::write(&foreign_note, "deploy").unwrap();

        grant_project_memory_read(&own, &foreign_repo);

        assert!(is_granted_project_memory_read(&own, &foreign_note));

        let git_meta = foreign_repo.join(".git").join("config");
        assert!(!is_granted_project_memory_read(&own, &git_meta));

        let system_note = foreign_repo.join("system").join("rules.md");
        assert!(!is_granted_project_memory_read(&own, &system_note));

        assert!(!is_granted_project_memory_read(&own, &foreign_repo));

        revoke_project_memory_reads(&own);
        assert!(!is_granted_project_memory_read(&own, &foreign_note));
    }
}
