use std::collections::BTreeMap;
use std::path::PathBuf;

use ulw_loop_core::paths::*;

#[test]
fn ulw_loop_dir_returns_repo_plus_omo_ulw_loop() {
    assert_eq!(ulw_loop_dir("/repo", None), PathBuf::from("/repo/.omo/ulw-loop"));
}

#[test]
fn ulw_loop_dir_scopes_artifacts_under_session() {
    let scope = UlwLoopScope {
        session_id: Some("sess_abc".into()),
        ..Default::default()
    };
    assert_eq!(ulw_loop_dir("/repo", Some(&scope)), PathBuf::from("/repo/.omo/ulw-loop/sess_abc"));
}

#[test]
fn resolve_ulw_loop_session_id_from_env_uses_pi_session_id() {
    let mut env = BTreeMap::new();
    env.insert("PI_SESSION_ID".into(), "01a00baf-feed-7f4d-a4a9-b8345c67cf72".into());
    assert_eq!(resolve_ulw_loop_session_id_from_env(&env), Some("01a00baf-feed-7f4d-a4a9-b8345c67cf72".into()));
}

#[test]
fn ulw_loop_paths_compose_artifact_filenames() {
    assert_eq!(ulw_loop_brief_path("/r", None), PathBuf::from("/r/.omo/ulw-loop/brief.md"));
    assert_eq!(ulw_loop_goals_path("/r", None), PathBuf::from("/r/.omo/ulw-loop/goals.json"));
    assert_eq!(ulw_loop_ledger_path("/r", None), PathBuf::from("/r/.omo/ulw-loop/ledger.jsonl"));

    let scope = UlwLoopScope {
        session_id: Some("session-A".into()),
        ..Default::default()
    };
    assert_eq!(ulw_loop_brief_path("/r", Some(&scope)), PathBuf::from("/r/.omo/ulw-loop/session-A/brief.md"));
    assert_eq!(ulw_loop_goals_path("/r", Some(&scope)), PathBuf::from("/r/.omo/ulw-loop/session-A/goals.json"));
    assert_eq!(ulw_loop_ledger_path("/r", Some(&scope)), PathBuf::from("/r/.omo/ulw-loop/session-A/ledger.jsonl"));
}

#[test]
fn normalize_ulw_loop_session_id_handles_traversal_and_blank() {
    assert_eq!(normalize_ulw_loop_session_id(Some("../bad/id")), Some("bad-id".into()));
    assert_eq!(normalize_ulw_loop_session_id(Some("  ")), None);
}

#[test]
fn repo_relative_strips_repo_prefix() {
    assert_eq!(repo_relative("/repo/.omo/ulw-loop/goals.json", "/repo"), ".omo/ulw-loop/goals.json");
    assert_eq!(repo_relative("/elsewhere/file", "/repo"), "/elsewhere/file");
}

#[test]
fn is_within_attempt_dir_checks_containment() {
    let posix_root = "/repo/.omo/evidence/ulw/s1/g1/a1";
    assert!(is_within_attempt_dir(&format!("{posix_root}/cli-pass.txt"), posix_root));
    assert!(is_within_attempt_dir(posix_root, posix_root));
    assert!(!is_within_attempt_dir("/repo/.omo/evidence/ulw/s1/g1/a1x/f.txt", posix_root));
    assert!(!is_within_attempt_dir("/repo/.omo/evidence/ulw/s1/g1/a0/f.txt", posix_root));
}

#[test]
fn normalize_session_id_uses_first_available_environment_key() {
    let mut env = BTreeMap::new();
    env.insert("CODEX_SESSION_ID".into(), "codex-session".into());
    env.insert("PI_SESSION_ID".into(), "pi-session".into());
    assert_eq!(resolve_ulw_loop_session_id_from_env(&env), Some("codex-session".into()));
}

#[test]
fn normalize_session_id_rejects_empty_environment_values() {
    let mut env = BTreeMap::new();
    env.insert("PI_SESSION_ID".into(), "  ".into());
    assert_eq!(resolve_ulw_loop_session_id_from_env(&env), None);
}

#[test]
fn normalize_session_id_removes_windows_traversal() {
    assert_eq!(normalize_ulw_loop_session_id(Some(r"..\bad\id")), Some("bad-id".into()));
}

#[test]
fn repo_relative_normalizes_windows_separators() {
    assert_eq!(repo_relative(r"C:\repo\.omo\goals.json", r"C:\repo"), ".omo/goals.json");
}

#[test]
fn repo_relative_does_not_strip_sibling_prefix() {
    assert_eq!(repo_relative("/repo-other/file", "/repo"), "/repo-other/file");
}

#[test]
fn attempt_dir_accepts_windows_child() {
    let root = r"C:\repo\.omo\evidence\ulw\s1\g1\a1";
    assert!(is_within_attempt_dir(r"C:\repo\.omo\evidence\ulw\s1\g1\a1\cli-pass.txt", root));
}

#[test]
fn attempt_dir_rejects_windows_sibling() {
    let root = r"C:\repo\.omo\evidence\ulw\s1\g1\a1";
    assert!(!is_within_attempt_dir(r"C:\repo\.omo\evidence\ulw\s1\g1\a1x\f.txt", root));
}

#[test]
fn attempt_dir_rejects_different_windows_drive() {
    let root = r"C:\repo\.omo\evidence\ulw\s1\g1\a1";
    assert!(!is_within_attempt_dir(r"D:\elsewhere\f.txt", root));
}
