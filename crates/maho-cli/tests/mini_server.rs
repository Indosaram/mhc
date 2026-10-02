use std::sync::Arc;
use maho_agent::harness::{context::BACKGROUND_CONTEXT, env::nodejs::NodeExecutionEnv, session::jsonl::{JsonlSessionRepo, JsonlSessionRepoOptions, JsonlSessionCreateOptions}};
#[tokio::test]
async fn server_lists_real_repository_metadata_without_opening_agent_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let root = dir.path().join("sessions").to_string_lossy().into_owned();
    let env = Arc::new(NodeExecutionEnv::new(&cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env, sessions_root: root.clone(), now: Some(Arc::new(|| 42)) });
    let session = repo.create(JsonlSessionCreateOptions { id: Some("known-session".to_owned()), cwd: cwd.clone(), parent_session_id: None }, &BACKGROUND_CONTEXT).await.unwrap();
    let expected = session.metadata().id.clone();
    repo.close(&BACKGROUND_CONTEXT).await;
    let sessions = maho_cli::experimental::mini::server::list_sessions(&root, &cwd).await.unwrap();
    assert_eq!(sessions.len(), 1); assert_eq!(sessions[0].id, expected); assert_eq!(sessions[0].cwd, cwd); assert_eq!(sessions[0].created_at, 42.0);
    assert!(maho_cli::experimental::mini::server::list_sessions(&dir.path().join("missing").to_string_lossy(), &cwd).await.unwrap().is_empty());
}
#[tokio::test]
async fn worker_opens_existing_session_by_id_and_creates_without_id() {
    use maho_cli::experimental::mini::worker::open_session;
    let dir = tempfile::tempdir().unwrap(); let cwd = dir.path().to_string_lossy().into_owned();
    let root = dir.path().join("sessions").to_string_lossy().into_owned();
    let env = Arc::new(NodeExecutionEnv::new(&cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env.clone(), sessions_root: root.clone(), now: Some(Arc::new(|| 42)) });
    let created = open_session(&repo, None, &cwd, &BACKGROUND_CONTEXT).await.unwrap();
    let id = created.metadata().id.clone(); repo.close(&BACKGROUND_CONTEXT).await;
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env, sessions_root: root, now: None });
    assert_eq!(open_session(&repo, Some(&id), "/different", &BACKGROUND_CONTEXT).await.unwrap().metadata().id, id);
    assert_eq!(open_session(&repo, Some("missing"), &cwd, &BACKGROUND_CONTEXT).await.err().unwrap(), "Unknown session: missing");
    repo.close(&BACKGROUND_CONTEXT).await;
}
