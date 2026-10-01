//! Port of senpi packages/agent/test/harness/jsonl-storage.test.ts.

mod support;

use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::env::nodejs::NodeExecutionEnv;
use maho_agent::harness::session::commit::{insert_entry, insert_usage};
use maho_agent::harness::session::jsonl::types::{JSONL_STORAGE_VERSION, JsonlStorageHeader, JsonlStorageOptions};
use maho_agent::harness::session::jsonl::JsonlStorage;
use maho_agent::harness::session::types::{
    EntryScan, NewEntry, NewUsageRow, ScanOrder, SessionStats, Storage, UsageScan,
};
use maho_agent::harness::session::values::{append_list, branch_tip, delete_list, list, set_value, session_name};
use maho_agent::harness::types::FileSystem;
use maho_ai::types::{Usage, UsageCost};

const NOW: i64 = 1_700_000_000_000;

fn temp_root(label: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "maho-jsonl-storage-{label}-{}-{}",
        std::process::id(),
        support::unique_suffix()
    ));
    std::fs::create_dir_all(&path).expect("temp root");
    path.to_string_lossy().into_owned()
}

fn file_system(root: &str) -> Arc<dyn FileSystem> {
    Arc::new(NodeExecutionEnv::new(root.to_owned()))
}

fn header(id: &str) -> JsonlStorageHeader {
    JsonlStorageHeader::new(id, JSONL_STORAGE_VERSION, NOW, "/workspace")
}

fn options(file_system: Arc<dyn FileSystem>, path: &str) -> JsonlStorageOptions {
    JsonlStorageOptions {
        file_system,
        path: path.to_owned(),
        now: Some(Arc::new(|| NOW)),
    }
}

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn user_entry(id: &str, content: &str) -> NewEntry {
    NewEntry::message(
        id,
        None,
        maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::User(maho_ai::types::UserMessage {
            content: maho_ai::types::UserContent::Text(content.to_owned()),
            timestamp: 1,
        })),
    )
}

#[tokio::test]
async fn replays_whole_list_deletion_without_resurrecting_earlier_appends() {
    let root = temp_root("list-delete");
    let file_system = file_system(&root);
    let options = options(file_system.clone(), "list-delete.jsonl");
    let events = list("test.events", "").expect("address");
    let storage = JsonlStorage::create(options.clone(), header("list-delete"), Vec::new(), &BACKGROUND_CONTEXT)
        .await
        .expect("create");
    storage
        .commit(
            vec![
                maho_agent::harness::session::types::Write::List(append_list(&events, serde_json::json!("first"))),
                maho_agent::harness::session::types::Write::List(append_list(&events, serde_json::json!("second"))),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("append");
    storage
        .commit(
            vec![maho_agent::harness::session::types::Write::List(delete_list(&events))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("delete");
    storage.close(&BACKGROUND_CONTEXT).await;

    let reopened = JsonlStorage::open(options, &BACKGROUND_CONTEXT).await.expect("reopen");
    assert!(reopened.read_list(&events, None, &BACKGROUND_CONTEXT).await.expect("list").is_empty());
    let recreated = reopened
        .commit(
            vec![maho_agent::harness::session::types::Write::List(append_list(
                &events,
                serde_json::json!("after"),
            ))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("recreate");
    assert_eq!(recreated.first_seq, 4);
    let elements = reopened.read_list(&events, None, &BACKGROUND_CONTEXT).await.expect("list");
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].seq, 4);
    assert_eq!(elements[0].value, serde_json::json!("after"));
    reopened.close(&BACKGROUND_CONTEXT).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn writes_one_line_per_transaction_and_replays_stamped_state() {
    let root = temp_root("round-trip");
    let file_system = file_system(&root);
    let options = options(file_system.clone(), "session.jsonl");
    let storage = JsonlStorage::create(options.clone(), header("round-trip"), Vec::new(), &BACKGROUND_CONTEXT)
        .await
        .expect("create");
    let committed = storage
        .commit(
            vec![
                insert_entry(user_entry("root", "hello")),
                maho_agent::harness::session::types::Write::Value(set_value(&branch_tip("main"), serde_json::json!("root"))),
                insert_usage(NewUsageRow {
                    id: "usage".to_owned(),
                    usage: usage(1, 2, 0, 0),
                    entry_id: Some("root".to_owned()),
                    adjustment: false,
                    details: None,
                }),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");
    storage
        .commit(
            vec![maho_agent::harness::session::types::Write::Value(set_value(
                &session_name(),
                serde_json::json!("name"),
            ))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("name");
    storage.close(&BACKGROUND_CONTEXT).await;

    let content = std::fs::read_to_string(format!("{root}/session.jsonl")).expect("content");
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[0]).expect("header"),
        serde_json::to_value(header("round-trip")).expect("header")
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[1]).expect("transaction").as_array().map(Vec::len),
        Some(3)
    );
    assert!(!serde_json::from_str::<serde_json::Value>(lines[2]).expect("transaction").is_array());

    let reopened = JsonlStorage::open(options, &BACKGROUND_CONTEXT).await.expect("reopen");
    let entries = reopened.get_entries(vec!["root".to_owned()], &BACKGROUND_CONTEXT).await.expect("entries");
    let root_entry = entries.get("root").expect("root entry");
    assert_eq!(root_entry.seq, committed.seqs[0]);
    assert_eq!(root_entry.timestamp, committed.timestamp);
    let stored = reopened
        .get_value(&branch_tip("main"), &BACKGROUND_CONTEXT)
        .await
        .expect("value")
        .expect("branch tip");
    assert_eq!(stored.value, serde_json::json!("root"));
    assert_eq!(stored.seq, committed.seqs[1]);
    let usage_rows = reopened
        .scan_usage(UsageScan { order: Some(ScanOrder::Asc), ..Default::default() }, &BACKGROUND_CONTEXT)
        .await
        .expect("usage");
    assert_eq!(usage_rows.len(), 1);
    assert_eq!(usage_rows[0].id, "usage");
    assert_eq!(usage_rows[0].seq, committed.seqs[2]);

    let historical = SessionStats {
        message_count: 1,
        usage: usage(1, 2, 0, 0),
    };
    assert_eq!(reopened.get_stats(&BACKGROUND_CONTEXT).await.expect("stats"), historical);
    let next = reopened.commit(Vec::new(), &BACKGROUND_CONTEXT).await.expect("next");
    assert_eq!(next.first_seq, 5);
    assert_eq!(next.stats, historical);
    reopened.close(&BACKGROUND_CONTEXT).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

async fn seed(root: &str) -> (Arc<dyn FileSystem>, JsonlStorageOptions, String) {
    let file_system = file_system(root);
    let options = options(file_system.clone(), "session.jsonl");
    let storage = JsonlStorage::create(options.clone(), header("torn"), Vec::new(), &BACKGROUND_CONTEXT)
        .await
        .expect("create");
    storage
        .commit(vec![insert_entry(user_entry("kept", "kept"))], &BACKGROUND_CONTEXT)
        .await
        .expect("commit");
    storage.close(&BACKGROUND_CONTEXT).await;
    let prefix = std::fs::read_to_string(format!("{root}/session.jsonl")).expect("prefix");
    (file_system, options, prefix)
}

#[tokio::test]
async fn discards_an_unterminated_final_object_line_and_truncates_before_admitting_writes() {
    let root = temp_root("torn-object");
    let (file_system, options, prefix) = seed(&root).await;
    let torn = serde_json::json!({
        "kind": "entry", "id": "torn", "parentId": null, "type": "message",
        "message": { "role": "user", "content": "torn", "timestamp": 1 },
        "seq": 2, "timestamp": NOW
    });
    file_system
        .append_file("session.jsonl", torn.to_string().as_bytes(), &BACKGROUND_CONTEXT)
        .await
        .expect("append torn");

    let reopened = JsonlStorage::open(options, &BACKGROUND_CONTEXT).await.expect("reopen");
    let entries = reopened
        .get_entries(vec!["kept".to_owned(), "torn".to_owned()], &BACKGROUND_CONTEXT)
        .await
        .expect("entries");
    assert!(!entries.contains_key("torn"));
    assert!(entries.contains_key("kept"));
    assert_eq!(std::fs::read_to_string(format!("{root}/session.jsonl")).expect("content"), prefix);
    assert!(!std::path::Path::new(&format!("{root}/session.jsonl.tmp")).exists());

    let next = reopened
        .commit(vec![insert_entry(user_entry("after", "after"))], &BACKGROUND_CONTEXT)
        .await
        .expect("commit");
    assert_eq!(next.first_seq, 2);
    reopened.close(&BACKGROUND_CONTEXT).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn discards_a_torn_array_line_wholly_including_list_elements() {
    let root = temp_root("torn-array");
    let (file_system, options, prefix) = seed(&root).await;
    let events = list("test.events", "").expect("address");
    let torn = serde_json::json!([
        {
            "kind": "entry", "id": "torn-a", "parentId": null, "type": "message",
            "message": { "role": "user", "content": "torn-a", "timestamp": 1 },
            "seq": 2, "timestamp": NOW
        },
        { "kind": "value", "op": "set", "seq": 3, "namespace": "pi.session.name", "key": "", "value": "lost" },
        { "kind": "list", "op": "append", "seq": 4, "namespace": "test.events", "key": "", "value": "lost" }
    ]);
    file_system
        .append_file("session.jsonl", torn.to_string().as_bytes(), &BACKGROUND_CONTEXT)
        .await
        .expect("append torn");

    let reopened = JsonlStorage::open(options, &BACKGROUND_CONTEXT).await.expect("reopen");
    assert!(!reopened
        .get_entries(vec!["torn-a".to_owned()], &BACKGROUND_CONTEXT)
        .await
        .expect("entries")
        .contains_key("torn-a"));
    assert!(reopened
        .get_value(&session_name(), &BACKGROUND_CONTEXT)
        .await
        .expect("value")
        .is_none());
    assert!(reopened.read_list(&events, None, &BACKGROUND_CONTEXT).await.expect("list").is_empty());
    assert_eq!(std::fs::read_to_string(format!("{root}/session.jsonl")).expect("content"), prefix);
    reopened.close(&BACKGROUND_CONTEXT).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn rejects_a_malformed_interior_line_without_rewriting() {
    let root = temp_root("malformed-interior");
    let (file_system, options, prefix) = seed(&root).await;
    let corrupted = format!(
        "{prefix}not-json\n{}\n",
        serde_json::json!({ "kind": "value", "op": "set", "seq": 2, "namespace": "pi.session.name", "key": "", "value": "after" })
    );
    file_system
        .write_file("session.jsonl", corrupted.as_bytes(), &BACKGROUND_CONTEXT)
        .await
        .expect("write");

    let error = JsonlStorage::open(options, &BACKGROUND_CONTEXT)
        .await
        .err()
        .expect("malformed");
    assert!(error.message.contains("line 3"), "{}", error.message);
    assert_eq!(std::fs::read_to_string(format!("{root}/session.jsonl")).expect("content"), corrupted);
    assert!(!std::path::Path::new(&format!("{root}/session.jsonl.tmp")).exists());
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn rejects_a_complete_malformed_final_line_without_rewriting() {
    let root = temp_root("malformed-final");
    let (file_system, options, prefix) = seed(&root).await;
    let corrupted = format!("{prefix}not-json\n");
    file_system
        .write_file("session.jsonl", corrupted.as_bytes(), &BACKGROUND_CONTEXT)
        .await
        .expect("write");

    let error = JsonlStorage::open(options, &BACKGROUND_CONTEXT)
        .await
        .err()
        .expect("malformed");
    assert!(error.message.contains("line 3"), "{}", error.message);
    assert_eq!(std::fs::read_to_string(format!("{root}/session.jsonl")).expect("content"), corrupted);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn rejects_a_complete_final_line_with_invalid_transaction_framing() {
    let root = temp_root("invalid-framing");
    let (file_system, options, prefix) = seed(&root).await;
    let corrupted = format!("{prefix}{}\n", serde_json::json!({ "kind": "nope", "seq": 2 }));
    file_system
        .write_file("session.jsonl", corrupted.as_bytes(), &BACKGROUND_CONTEXT)
        .await
        .expect("write");

    let error = JsonlStorage::open(options, &BACKGROUND_CONTEXT)
        .await
        .err()
        .expect("invalid framing");
    assert!(error.message.contains("line 3"), "{}", error.message);
    assert_eq!(std::fs::read_to_string(format!("{root}/session.jsonl")).expect("content"), corrupted);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn rejects_an_unterminated_header() {
    let root = temp_root("torn-header");
    let file_system = file_system(&root);
    let options = options(file_system.clone(), "session.jsonl");
    let serialized = serde_json::to_string(&header("torn")).expect("header");
    file_system
        .write_file(
            "session.jsonl",
            &serialized.as_bytes()[..serialized.len() - 4],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("write");

    let error = JsonlStorage::open(options, &BACKGROUND_CONTEXT)
        .await
        .err()
        .expect("torn header");
    assert!(error.message.contains("missing header"), "{}", error.message);
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[tokio::test]
async fn scan_entries_orders_and_filters_stamped_entries() {
    let root = temp_root("scan-entries");
    let file_system = file_system(&root);
    let options = options(file_system.clone(), "session.jsonl");
    let storage = JsonlStorage::create(options, header("scan"), Vec::new(), &BACKGROUND_CONTEXT)
        .await
        .expect("create");
    storage
        .commit(
            vec![
                insert_entry(user_entry("a", "a")),
                insert_entry(user_entry("b", "b")),
                insert_entry(user_entry("c", "c")),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("commit");

    let ascending = storage
        .scan_entries(
            EntryScan {
                order: Some(ScanOrder::Asc),
                limit: Some(2),
                ..Default::default()
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("ascending");
    assert_eq!(
        ascending.iter().map(|entry| entry.id.as_str()).collect::<Vec<&str>>(),
        vec!["a", "b"]
    );
    let descending = storage
        .scan_entries(
            EntryScan {
                order: Some(ScanOrder::Desc),
                ..Default::default()
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("descending");
    assert_eq!(
        descending.iter().map(|entry| entry.id.as_str()).collect::<Vec<&str>>(),
        vec!["c", "b", "a"]
    );
    storage.close(&BACKGROUND_CONTEXT).await;
    std::fs::remove_dir_all(&root).expect("cleanup");
}
