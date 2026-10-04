use maho_server::app_server::archive_state::ThreadArchiveState;
use serde_json::json;

#[tokio::test]
async fn archive_roundtrip_touches_session_and_preserves_other_archive() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    std::fs::write(&path,"session").unwrap();
    let state = ThreadArchiveState::new(Some(directory.path().into()));
    let thread = json!({"id":"t","sessionId":"t","cwd":"/tmp","createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z","status":{"type":"idle"},"sessionPath":path,"name":null,"preview":"text"});
    state.mark_archived(&thread,"2026-01-01T00:00:00.000Z").await.unwrap();
    assert!(state.is_archived(&thread).await.unwrap());
    assert_eq!(state.list_archived_threads().await.unwrap(), vec![thread.clone()]);
    let restored = state.unarchive("t",0).await.unwrap().unwrap();
    assert_eq!(restored["id"],"t");
    assert_ne!(restored["updatedAt"],thread["updatedAt"]);
    assert!(!state.is_archived(&thread).await.unwrap());
    assert!(state.unarchive("t",0).await.unwrap().is_none());
    state.mark_archived(&thread,"2026-01-01T00:00:00.000Z").await.unwrap();
    state.clear_archived("other").await.unwrap();
    assert!(state.is_archived(&thread).await.unwrap());
    state.clear_archived("t").await.unwrap();
    assert!(state.list_archived_threads().await.unwrap().is_empty());
}
