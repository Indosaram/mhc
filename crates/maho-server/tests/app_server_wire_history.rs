use maho_server::app_server::wire_thread::{logged_turn_with_view,turns_from_session_entries};
use serde_json::json;

#[test]
fn persisted_messages_reconstruct_turns_and_summary_views() {
    let entries = vec![json!({"type":"message","id":"u","timestamp":"2020-01-01T00:00:00.000Z","message":{"role":"user","content":"hello"}}),json!({"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"first"}]}}),json!({"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"text","text":"last"}]}})];
    let turns = turns_from_session_entries(&entries,"fallback");
    assert_eq!(turns.len(),1);
    assert_eq!(turns[0].turn_id,"turn-1");
    assert_eq!(logged_turn_with_view(&turns[0],"full")["items"].as_array().unwrap().len(),3);
    let summary = logged_turn_with_view(&turns[0],"summary");
    assert_eq!(summary["items"][0]["id"],"u");
    assert_eq!(summary["items"][1]["id"],"a2");
    assert!(logged_turn_with_view(&turns[0],"notLoaded")["items"].as_array().unwrap().is_empty());
}

#[test]
fn logged_turn_accepts_non_rfc3339_started_and_completed_at() {
    use maho_server::app_server::turn_log::{LoggedTurn,TurnStatus};
    let turn = LoggedTurn {turn_id:"t".into(),started_at:"2020-01-01".into(),completed_at:Some("2020-01-01 00:00:02".into()),duration_ms:None,error:None,status:TurnStatus::Completed,items:Vec::new()};
    let wire = logged_turn_with_view(&turn,"full");
    assert_eq!(wire["startedAt"],1577836800.0);
    assert_eq!(wire["completedAt"],1577836802.0);
}

#[tokio::test]
async fn unloaded_history_reads_disk_without_creating_a_native_session() {
    use maho_server::app_server::{archive_state::ThreadArchiveState,history_handlers::thread_history_turns,thread_registry::ThreadRegistry,turn_log::TurnLog};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let records = [json!({"type":"session","id":"disk","version":3,"cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}),json!({"type":"message","id":"u","parentId":null,"timestamp":"2020-01-01T00:00:01.000Z","message":{"role":"user","content":"hello"}}),json!({"type":"message","id":"a","parentId":"u","timestamp":"2020-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"persisted"}]}})];
    std::fs::write(path,records.map(|record|record.to_string()).join("\n")).unwrap();
    let registry = ThreadRegistry::new(directory.path().display().to_string(),Some(directory.path().display().to_string()),None);
    let archive = ThreadArchiveState::new(Some(directory.path().into()));
    let log = tokio::sync::Mutex::new(TurnLog::default());
    let turns = thread_history_turns("disk",&registry,&archive,&log).await.unwrap();
    assert_eq!(turns.len(),1);
    assert_eq!(turns[0].items[1]["text"],"persisted");
    assert!(registry.list_loaded().await.is_empty());
}
