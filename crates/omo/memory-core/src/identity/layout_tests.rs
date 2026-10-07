use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

fn env_with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn all_paths(paths: &MemoryIdentityPaths) -> Vec<PathBuf> {
    vec![
        paths.root.clone(),
        paths.repo.clone(),
        paths.runtime.clone(),
        paths.locks.clone(),
        paths.transcripts.clone(),
        paths.reflection.clone(),
        paths.reflection_sessions.clone(),
        paths.worktrees.clone(),
        paths.viewers.clone(),
        paths.push_queue.clone(),
        paths.facts_queue.clone(),
        paths.facts.clone(),
        paths.notices.clone(),
        paths.tool_receipts.clone(),
        paths.recall.clone(),
    ]
}

#[test]
fn test_runtime_subdirnames_when_enumerated_then_every_declared_subdir_lives_under_runtime() {
    let paths = build_identity_paths(Path::new("/mem"), "abc-0123abcd");
    let all = all_paths(&paths);
    for subdir in RUNTIME_SUBDIRNAMES {
        assert!(
            all.contains(&paths.runtime.join(subdir)),
            "missing runtime subdir {subdir}"
        );
    }
}

#[test]
fn test_default_memory_root_when_no_override_then_it_is_home_dot_omo_memory() {
    let expected = home_directory().join(".maho").join("memory");
    assert_eq!(default_memory_root(), expected);
}

#[test]
fn test_resolve_memory_root_when_override_is_absolute_then_it_is_used_verbatim() {
    let override_root = std::env::temp_dir().join("qa-memory-home");
    let env = env_with(&[(MEMORY_ROOT_ENV_VAR, override_root.to_str().unwrap())]);
    assert_eq!(
        resolve_memory_root(&env, Path::new("/work/proj")),
        override_root
    );
}

#[test]
fn test_resolve_memory_root_when_override_is_relative_then_it_resolves_against_cwd() {
    let env = env_with(&[(MEMORY_ROOT_ENV_VAR, "qa-home")]);
    assert_eq!(
        resolve_memory_root(&env, Path::new("/work/proj")),
        Path::new("/work/proj/qa-home")
    );
}

#[test]
fn test_resolve_memory_root_when_override_is_blank_then_default_root_is_used() {
    let expected = default_memory_root();
    assert_eq!(
        resolve_memory_root(&env_with(&[(MEMORY_ROOT_ENV_VAR, "")]), Path::new("/w")),
        expected
    );
    assert_eq!(
        resolve_memory_root(&env_with(&[(MEMORY_ROOT_ENV_VAR, "   ")]), Path::new("/w")),
        expected
    );
    assert_eq!(
        resolve_memory_root(&BTreeMap::new(), Path::new("/w")),
        expected
    );
}

#[test]
fn test_build_identity_paths_when_root_and_id_given_then_layout_shape_is_produced() {
    let paths = build_identity_paths(Path::new("/mem"), "backend-lead-0123abcd");
    let root = Path::new("/mem/agents/backend-lead-0123abcd");
    let runtime = root.join("runtime");
    assert_eq!(paths.root, root);
    assert_eq!(paths.repo, root.join("repo"));
    assert_eq!(paths.runtime, runtime);
    assert_eq!(paths.locks, runtime.join("locks"));
    assert_eq!(paths.transcripts, runtime.join("transcripts"));
    assert_eq!(paths.reflection, runtime.join("reflection"));
    assert_eq!(
        paths.reflection_sessions,
        runtime.join("reflection-sessions")
    );
    assert_eq!(paths.worktrees, runtime.join("worktrees"));
    assert_eq!(paths.viewers, runtime.join("viewers"));
    assert_eq!(paths.push_queue, runtime.join("push-queue"));
    assert_eq!(paths.facts_queue, runtime.join("facts-queue"));
    assert_eq!(paths.facts, runtime.join("facts"));
    assert_eq!(paths.notices, runtime.join("notices"));
    assert_eq!(paths.tool_receipts, runtime.join("tool-receipts"));
    assert_eq!(paths.recall, runtime.join("recall"));
    assert_eq!(paths.recall_ledger, runtime.join("recall").join("ledger"));
    assert_eq!(paths.recall_pending, runtime.join("recall").join("pending"));
}

#[test]
fn test_lock_domain_layout_when_identity_is_built_then_lock_dir_stays_inside_the_root() {
    let paths = build_identity_paths(Path::new("/mem"), "id");
    assert!(paths.locks.starts_with(&paths.root));
    assert!(paths.facts_queue.starts_with(&paths.root));
}
