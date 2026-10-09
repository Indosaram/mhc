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
