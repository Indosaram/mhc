use maho_server::app_server::search_cache::{ThreadSearchCache,parse_search_session};
use serde_json::json;

#[test]
fn parser_distinguishes_user_recency_from_assistant_activity() {
    let contents = [json!({"type":"session","id":"s","cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}),json!({"type":"message","timestamp":1700000000000_i64,"message":{"role":"user","content":[{"type":"text","text":"hello"},{"type":"image"}]}}),json!({"type":"message","timestamp":1800000000000_i64,"message":{"role":"assistant","content":"answer"}}),json!({"type":"session_info","name":" name "})].map(|entry|entry.to_string()).join("\n");
    let record = parse_search_session("session.jsonl",0,&contents).unwrap();
    assert_eq!(record.searchable_text,"hello answer");
    assert_eq!(record.thread["preview"],"hello");
    assert_eq!(record.thread["name"],"name");
    assert_ne!(record.thread["updatedAt"],record.recency_at);
    assert!(parse_search_session("bad",0,"{\"type\":\"message\"}").is_none());
}

#[test]
fn parser_accepts_non_rfc3339_session_timestamps() {
    let contents = [json!({"type":"session","id":"s","cwd":"/work","timestamp":"2020-01-01"}),json!({"type":"message","timestamp":"2020-01-01 00:00:05","message":{"role":"user","content":"hi"}})].map(|entry|entry.to_string()).join("\n");
    let record = parse_search_session("session.jsonl",0,&contents).unwrap();
    assert_eq!(record.thread["createdAt"],"2020-01-01T00:00:00.000Z");
    assert_eq!(record.thread["updatedAt"],"2020-01-01T00:00:05.000Z");
}

#[tokio::test]
async fn cache_invalidates_submillisecond_file_modification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let first = json!({"type":"session","id":"first","cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}).to_string();
    let second = json!({"type":"session","id":"second","cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}).to_string();
    let baseline = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1700000000);
    std::fs::write(&path, first).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(baseline)).unwrap();
    let mut cache = ThreadSearchCache::default();
    assert_eq!(cache.load_file(&path).await.unwrap().unwrap().thread["id"], "first");
    std::fs::write(&path, second).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(baseline + std::time::Duration::from_nanos(500000))).unwrap();
    assert_eq!(cache.load_file(&path).await.unwrap().unwrap().thread["id"], "second");
    assert_eq!(cache.stats().misses, 2);
}

#[tokio::test]
async fn cache_hits_evicts_and_removes_deleted_files_without_timing_delays() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("one.jsonl");let second = directory.path().join("two.jsonl");
    let contents = json!({"type":"session","id":"s","cwd":"/work","timestamp":"2020-01-01T00:00:00.000Z"}).to_string();
    std::fs::write(&first,&contents).unwrap();std::fs::write(&second,&contents).unwrap();
    let mut cache = ThreadSearchCache::new(1);
    cache.load_file(&first).await.unwrap();cache.load_file(&first).await.unwrap();
    assert_eq!(cache.stats().hits,1);
    cache.load_file(&second).await.unwrap();assert_eq!(cache.stats().entries,1);
    cache.load_file(&first).await.unwrap();assert_eq!(cache.stats().misses,3);
    std::fs::remove_file(&first).unwrap();assert!(cache.load_file(&first).await.unwrap().is_none());
    assert_eq!(cache.stats().entries,0);
}
