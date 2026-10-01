//! Port of senpi packages/agent/test/harness/memory-storage.test.ts.

use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::session::commit::{insert_entry, insert_usage};
use maho_agent::harness::session::types::NewEntry;
use maho_agent::harness::session::values::{set_value, session_name};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, Storage};

const NOW: i64 = 1_700_000_000_000;

#[tokio::test]
async fn uses_the_injected_clock_once_per_transaction() {
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(NOW));
    let storage = MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new({
            let clock = clock.clone();
            move || clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        })),
    });

    let first = storage
        .commit(
            vec![
                insert_entry(NewEntry::custom("first", None, "note")),
                maho_agent::harness::session::types::Write::Value(set_value(&session_name(), serde_json::json!("first"))),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("first commit");
    let second = storage
        .commit(
            vec![maho_agent::harness::session::types::Write::Value(set_value(
                &session_name(),
                serde_json::json!("second"),
            ))],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("second commit");

    assert_eq!(first.timestamp, NOW);
    assert_eq!(second.timestamp, NOW + 1);
    let entries = storage.get_entries(vec!["first".to_owned()], &BACKGROUND_CONTEXT).await.expect("entries");
    assert_eq!(entries.get("first").expect("first entry").timestamp, first.timestamp);
    let _ = insert_usage;
    storage.close(&BACKGROUND_CONTEXT).await;
}
