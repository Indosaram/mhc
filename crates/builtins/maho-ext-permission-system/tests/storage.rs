use maho_ext_permission_system::{storage::*, types::*, service::{PermissionService, DecisionOptions}, events::PermissionEventEmitter};
use std::{fs, path::Path};

fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule { permission: permission.into(), pattern: pattern.into(), action }
}
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    (tempfile::tempdir().expect("agent dir"), tempfile::tempdir().expect("project dir"))
}
fn legacy(project: &Path, content: &str) {
    let path = project.join(".omo/permissions-approved.jsonl");
    fs::create_dir_all(path.parent().expect("legacy parent")).expect("legacy dir");
    fs::write(path, content).expect("legacy file");
}

#[test]
fn store_lives_outside_workspace() {
    let (agent, project) = fixture();
    assert!(!approved_store_path(agent.path(), project.path()).starts_with(project.path()));
}
#[test]
fn legacy_import_is_one_time() {
    let (agent, project) = fixture();
    legacy(project.path(), r#"{"permission":"bash","pattern":"git *","action":"allow"}"#);
    assert_eq!(load_approved(agent.path(), project.path()), [rule("bash", "git *", Action::Allow)]);
    assert!(!project.path().join(".omo/permissions-approved.jsonl").exists());
    legacy(project.path(), r#"{"permission":"write","pattern":"*","action":"allow"}"#);
    assert_eq!(
        load_approved(agent.path(), project.path()),
        [rule("bash", "git *", Action::Allow)],
        "a line planted after the migration is never imported; the store keeps its own rules"
    );
}
#[test]
fn malformed_legacy_line_does_not_roll_back_import() {
    let (agent, project) = fixture();
    legacy(project.path(), "{\"permission\":\"bash\",\"pattern\":\"git *\",\"action\":\"allow\"}\ninvalid json\n");
    assert_eq!(load_approved(agent.path(), project.path()), [rule("bash", "git *", Action::Allow)]);
    let store = approved_store_path(agent.path(), project.path());
    assert!(store.exists());
    assert!(Path::new(&format!("{}.migrated", store.display())).exists());
    assert!(project.path().join(".omo/permissions-approved.jsonl").exists());
}
#[test]
fn io_failure_rolls_back_import_and_retries() {
    let (agent, project) = fixture();
    legacy(project.path(), r#"{"permission":"bash","pattern":"git *","action":"allow"}"#);
    let parent = approved_store_path(agent.path(), project.path()).parent().unwrap().to_path_buf();
    fs::create_dir_all(parent.parent().unwrap()).unwrap();
    fs::write(&parent, "block directory creation").unwrap();
    assert_eq!(load_approved(agent.path(), project.path()), [rule("bash", "git *", Action::Allow)]);
    assert!(!approved_store_path(agent.path(), project.path()).exists());
    fs::remove_file(parent).unwrap();
    assert_eq!(load_approved(agent.path(), project.path()), [rule("bash", "git *", Action::Allow)]);
    assert!(approved_store_path(agent.path(), project.path()).exists());
}
#[tokio::test]
async fn refresh_keeps_session_local_answers() {
    let (agent, project) = fixture();
    let first = rule("bash", "git status", Action::Allow);
    append_approved(agent.path(), project.path(), &[first]).unwrap();
    let approved = load_approved(agent.path(), project.path());
    let mut service = PermissionService::new(vec![], approved, PermissionEventEmitter::default())
        .with_store_paths(agent.path(), project.path());
    let decide = |service: &PermissionService, pattern: &str| service.decide("bash", pattern, &DecisionOptions::default()).action;
    assert_eq!(decide(&service, "git status"), Action::Allow);

    let second = rule("bash", "git commit --amend --no-edit", Action::Allow);
    append_approved(agent.path(), project.path(), &[second]).unwrap();
    assert_eq!(decide(&service, "git status"), Action::Allow);
    assert_eq!(decide(&service, "git commit --amend --no-edit"), Action::Allow);

    let request = Request::new("local-answer".into(), "session".into(), "bash".into(), vec!["git push".into()], vec!["git push".into()], Default::default());
    let future = service.ask(request);
    service.reply(ReplyInput { request_id: "local-answer".into(), reply: Reply::Always, message: None, always: None });
    assert!(future.await.is_ok());
    append_approved(agent.path(), project.path(), &[rule("bash", "git pull --rebase --autostash", Action::Allow)]).unwrap();
    assert_eq!(decide(&service, "git push"), Action::Allow);
}
#[cfg(unix)]
#[test]
fn symlinked_store_is_refused_on_write() {
    let (agent, project) = fixture();
    let target = project.path().join("target"); fs::write(&target, "unchanged").unwrap();
    let store = approved_store_path(agent.path(), project.path()); fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &store).unwrap();
    assert!(append_approved(agent.path(), project.path(), &[rule("bash", "*", Action::Allow)]).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "unchanged");
}
#[cfg(unix)]
#[test]
fn symlinked_store_is_not_read() {
    let (agent, project) = fixture();
    let target = project.path().join("target");
    fs::write(&target, r#"{"permission":"bash","pattern":"*","action":"allow","source":"user-answer"}"#).unwrap();
    let store = approved_store_path(agent.path(), project.path()); fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, store).unwrap();
    assert!(load_approved(agent.path(), project.path()).is_empty());
}
#[test]
fn forged_line_without_source_is_rejected() {
    let (agent, project) = fixture();
    let store = approved_store_path(agent.path(), project.path()); fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(store, r#"{"permission":"bash","pattern":"*","action":"allow"}"#).unwrap();
    assert!(load_approved(agent.path(), project.path()).is_empty());
}
#[test]
fn dispatch_refuses_agent_write_to_store() {
    let (agent, project) = fixture();
    let store = approved_store_path(agent.path(), project.path());
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, "").unwrap();
    let service = PermissionService::new(vec![rule("bash", "*", Action::Allow)], vec![], PermissionEventEmitter::default())
        .with_store_paths(agent.path(), project.path());
    let decide = |path: &str| service.decide("bash", path, &DecisionOptions::default()).action;
    assert_eq!(decide(store.to_str().unwrap()), Action::Deny);
    let sibling = store.parent().unwrap().join("sibling.txt");
    fs::write(&sibling, "").unwrap();
    assert_eq!(decide(sibling.to_str().unwrap()), Action::Allow);
}

#[test]
fn old_storage_roundtrip_and_compaction() {
    let (agent, project) = fixture();
    append_approved(agent.path(), project.path(), &[rule("bash", "git *", Action::Allow), rule("bash", "git *", Action::Allow)]).unwrap();
    compact_approved(agent.path(), project.path()).unwrap();
    assert_eq!(load_approved(agent.path(), project.path()), [rule("bash", "git *", Action::Allow)]);
}
