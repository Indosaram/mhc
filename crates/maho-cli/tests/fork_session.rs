use maho_cli::cli::runtime::fork_session;
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
    let source_path = source.session_file().expect("source path").to_owned();

    let target_cwd = directory.path().join("target");
    let target_cwd_text = target_cwd.to_string_lossy().into_owned();
    let forked = fork_session(&source_path, &target_cwd_text, Some(&session_dir_text), None).expect("fork");

    assert_ne!(forked.session_id(), source.session_id());
    assert_eq!(forked.cwd(), target_cwd_text);
    assert_eq!(forked.header().expect("header")["parentSession"], source_path);
    assert_eq!(forked.entries(), source.entries());
}
