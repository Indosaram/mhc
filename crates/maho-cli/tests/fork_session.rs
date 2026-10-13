use maho_cli::cli::runtime::{find_local_session_by_exact_id, fork_session, resolve_session_dir, resolve_session_path, ResolvedSession};
use maho_core::session_manager::SessionManager;

#[test]
fn forks_history_into_new_session_with_parent_source() {
    let directory = tempfile::tempdir().expect("directory");
    let cwd = directory.path().join("source");
    let session_dir = directory.path().join("sessions");
    let cwd_text = cwd.to_string_lossy().into_owned();
    let session_dir_text = session_dir.to_string_lossy().into_owned();
    let mut source = SessionManager::create(&cwd_text, Some(&session_dir_text), Some(maho_core::session_manager::NewSessionOptions {
        id: Some("source-session".to_owned()), ..Default::default()
    }));
    source.append_message(serde_json::json!({"role":"user", "content":"hello"}));
    source.append_message(serde_json::json!({"role":"assistant","content":[],"stopReason":"stop","timestamp":2}));
    let source_path = source.session_file().expect("source path").to_owned();

    let target_cwd = directory.path().join("target");
    let target_cwd_text = target_cwd.to_string_lossy().into_owned();
    let forked = fork_session(&source_path, &target_cwd_text, Some(&session_dir_text), None).expect("fork");

    assert_ne!(forked.session_id(), source.session_id());
    assert_eq!(forked.cwd(), target_cwd_text);
    assert_eq!(forked.header().expect("header")["parentSession"], source_path);
    assert_eq!(forked.entries(), source.entries());
    let original = std::fs::read(&source_path).expect("source bytes");
    let by_prefix = fork_session("source-", &target_cwd_text, Some(&session_dir_text), None).expect("prefix fork");
    assert_eq!(by_prefix.entries(), source.entries());
    assert_ne!(by_prefix.session_id(), source.session_id());
    assert_eq!(std::fs::read(&source_path).expect("unchanged source"), original);
    assert_eq!(fork_session("missing-id", &target_cwd_text, Some(&session_dir_text), None).expect_err("unknown ID"), "No session found matching 'missing-id'");
    assert_eq!(fork_session(&source_path, &target_cwd_text, Some(&session_dir_text), Some("source-session")).expect_err("duplicate target"), "Session already exists with id 'source-session'");
    let mut local = SessionManager::create(&target_cwd_text, Some(&session_dir_text), Some(maho_core::session_manager::NewSessionOptions {
        id: Some("source-local".to_owned()), ..Default::default()
    }));
    local.append_message(serde_json::json!({"role":"user","content":"local priority"}));
    local.append_message(serde_json::json!({"role":"assistant","content":[],"stopReason":"stop","timestamp":3}));
    let prioritized = fork_session("source-", &target_cwd_text, Some(&session_dir_text), None).expect("local prefix priority");
    assert_eq!(prioritized.entries(), local.entries());
    let global = fork_session("source-session", &target_cwd_text, Some(&session_dir_text), None).expect("custom global fallback");
    assert_eq!(global.entries(), source.entries());
    assert_eq!(fork_session(&source_path, &target_cwd_text, Some(&session_dir_text), Some("source-local")).expect_err("distinct duplicate target"), "Session already exists with id 'source-local'");
}

#[test]
fn session_dir_prefers_flag_then_env_then_setting_and_expands_tilde() {
    let home = maho_core::config::home_dir();
    assert_eq!(resolve_session_dir(Some("~/flag"), Some("/env"), Some("/setting")), Some(format!("{}/flag", home.trim_end_matches('/'))));
    assert_eq!(resolve_session_dir(None, Some("~/env"), Some("/setting")), Some(format!("{}/env", home.trim_end_matches('/'))));
    assert_eq!(resolve_session_dir(None, Some(""), Some("/setting")).as_deref(), Some("/setting"));
    assert_eq!(resolve_session_dir(None, None, None), None);
}

#[test]
fn session_argument_resolves_paths_then_local_then_global_ids() {
    let directory = tempfile::tempdir().expect("directory");
    let session_dir = directory.path().join("sessions").to_string_lossy().into_owned();
    let here = directory.path().join("here").to_string_lossy().into_owned();
    let there = directory.path().join("there").to_string_lossy().into_owned();
    let options = |id: &str| Some(maho_core::session_manager::NewSessionOptions { id: Some(id.to_owned()), ..Default::default() });
    let mut local = SessionManager::create(&here, Some(&session_dir), options("local-session"));
    let mut remote = SessionManager::create(&there, Some(&session_dir), options("remote-session"));
    for manager in [&mut local, &mut remote] {
        manager.append_message(serde_json::json!({"role":"user","content":"hello"}));
        manager.append_message(serde_json::json!({"role":"assistant","content":[],"stopReason":"stop","timestamp":2}));
    }
    let local_path = local.session_file().expect("local path").to_owned();
    let remote_path = remote.session_file().expect("remote path").to_owned();

    assert_eq!(resolve_session_path("local-", &here, Some(&session_dir)), ResolvedSession::Local(local_path.clone()));
    assert_eq!(resolve_session_path("remote", &here, Some(&session_dir)), ResolvedSession::Global { path: remote_path, cwd: there });
    assert_eq!(resolve_session_path(&local_path, &here, Some(&session_dir)), ResolvedSession::Path(local_path.clone()));
    assert_eq!(resolve_session_path("absent", &here, Some(&session_dir)), ResolvedSession::NotFound("absent".to_owned()));
    assert_eq!(find_local_session_by_exact_id("local-session", &here, Some(&session_dir)), Some(local_path));
    assert_eq!(find_local_session_by_exact_id("local", &here, Some(&session_dir)), None);
}
