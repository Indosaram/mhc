use maho_agent::harness::{context::background_context, session::*};
use maho_server::sqlite::{
    apply_initial_schema, session_row::insert_session, storage::SqliteStorage,
};
use serde_json::json;
#[tokio::test]
async fn durable_storage_reopens_and_failed_commit_rolls_back_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    apply_initial_schema(&db).unwrap();
    let metadata = SessionMetadata {
        id: "s".into(),
        created_at: 0,
        storage_version: 1,
        cwd: None,
        parent_session_id: None,
        legacy_parent_session_path: None,
    };
    insert_session(&db, &metadata, 1).unwrap();
    let context = background_context();
    let storage = SqliteStorage::new(db, "s".into(), Box::new(|| 123));
    let address = value("test", "key").unwrap();
    let result = storage
        .commit(
            vec![
                insert_entry(NewEntry::custom("root", None, "test")),
                Write::Value(ValueWrite::Set(ValueSetWrite {
                    namespace: address.namespace.clone(),
                    key: address.key.clone(),
                    value: json!(null),
                })),
            ],
            &context,
        )
        .await
        .unwrap();
    assert_eq!(result.seqs, vec![1, 2]);
    assert_eq!(result.timestamp, 123);
    let failure = storage
        .commit(
            vec![
                insert_entry(NewEntry::custom("child", Some("root".into()), "test")),
                insert_entry(NewEntry::custom("root", None, "duplicate")),
            ],
            &context,
        )
        .await;
    assert!(failure.is_err());
    assert!(
        storage
            .get_entries(vec!["child".into()], &context)
            .await
            .unwrap()
            .is_empty()
    );
    let result = storage
        .commit(
            vec![insert_entry(NewEntry::custom(
                "child",
                Some("root".into()),
                "test",
            ))],
            &context,
        )
        .await
        .unwrap();
    assert_eq!(result.first_seq, 3);
    let snapshot=storage.snapshot(&ForkOptions::Tree {id:None}).unwrap();
    assert_eq!(snapshot.entries.len(),2);
    assert_eq!(snapshot.entries_complete,Some(true));
    assert!(storage.snapshot(&ForkOptions::Branch {branch:"missing".into(),entry_id:None,position:None,id:None}).is_err());
    assert_eq!(
        storage
            .scan_branch(StorageBranchScan::new("child"), &context)
            .await
            .unwrap()
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["child", "root"]
    );
    storage.close(&context).await;
    assert!(storage.snapshot(&ForkOptions::Tree {id:None}).is_err());
    assert!(storage.get_stats(&context).await.is_err());
    drop(storage);
    let storage = SqliteStorage::new(
        rusqlite::Connection::open(&path).unwrap(),
        "s".into(),
        Box::new(|| 124),
    );
    assert_eq!(
        storage
            .get_value(&address, &context)
            .await
            .unwrap()
            .unwrap()
            .value,
        json!(null)
    );
    assert_eq!(
        storage
            .scan_entries(EntryScan::default(), &context)
            .await
            .unwrap()
            .len(),
        2
    );
    let result = storage
        .commit(
            vec![insert_entry(NewEntry::custom(
                "next",
                Some("child".into()),
                "test",
            ))],
            &context,
        )
        .await
        .unwrap();
    assert_eq!(result.first_seq, 4);
    storage.close(&context).await;
}
