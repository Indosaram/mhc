//! Port of senpi packages/agent/test/harness/session-create-branch.test.ts.

mod support;

use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::session::commit::insert_entry;
use maho_agent::harness::session::types::{NewEntry, Session, SessionReader, Storage};
use maho_agent::harness::session::values::{branch_tip, lane_config, lane_state};
use maho_agent::harness::session::{MemoryStorage, MemoryStorageOptions, SessionErrorKind, StorageBackedSession, StorageBackedSessionOptions};

fn create_session() -> Arc<StorageBackedSession> {
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| 10)),
    }));
    let session = Arc::new(StorageBackedSession::new(
        maho_agent::harness::session::types::SessionMetadata {
            id: "session".to_owned(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
        },
        storage,
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    session
}

#[tokio::test]
async fn creates_only_the_data_branch_at_a_validated_target() {
    let context = BACKGROUND_CONTEXT.clone();
    let session = create_session();
    session
        .mutate(
            Arc::new(|mutator, context| {
                Box::pin(async move {
                    mutator
                        .commit(vec![insert_entry(NewEntry::custom("target", None, "target"))], context)
                        .await?;
                    Ok(serde_json::Value::Null)
                })
            }),
            &context,
        )
        .await
        .expect("seed target");

    let branch = session.create_branch("main", Some("target".to_owned()), &context).await.expect("branch");
    assert_eq!(branch.get_tip_id(&context).await.expect("tip"), Some("target".to_owned()));
    let tip = session
        .get_value(&branch_tip("main"), &context)
        .await
        .expect("value")
        .expect("tip value");
    assert_eq!(tip.value, serde_json::json!("target"));
    assert!(session.get_value(&lane_config("main"), &context).await.expect("config").is_none());
    assert!(session.get_value(&lane_state("main"), &context).await.expect("state").is_none());
    session.close(&context).await;
}

#[tokio::test]
async fn validates_names_and_non_null_targets() {
    let context = BACKGROUND_CONTEXT.clone();
    let session = create_session();
    let empty = session
        .create_branch("", None, &context)
        .await
        .err()
        .expect("empty name");
    assert_eq!(empty.kind, SessionErrorKind::InvalidBranch);
    let nul = session
        .create_branch("bad\u{0}name", None, &context)
        .await
        .err()
        .expect("nul name");
    assert_eq!(nul.kind, SessionErrorKind::InvalidBranch);
    let missing = session
        .create_branch("main", Some("missing".to_owned()), &context)
        .await
        .err()
        .expect("missing target");
    assert_eq!(missing.kind, SessionErrorKind::UnknownTarget);
    assert!(session.branch("main", &context).await.expect("branch").is_none());
    session.close(&context).await;
}

#[tokio::test]
async fn rejects_duplicates_atomically_including_concurrent_creation() {
    let context = BACKGROUND_CONTEXT.clone();
    let session = create_session();
    let first = session.create_branch("main", None, &context);
    let second = session.create_branch("main", None, &context);
    let (first, second) = tokio::join!(first, second);
    let fulfilled = [first.is_ok(), second.is_ok()].iter().filter(|ok| **ok).count();
    assert_eq!(fulfilled, 1);
    let rejected = first.err().or_else(|| second.err()).expect("one rejection");
    assert_eq!(rejected.kind, SessionErrorKind::BranchExists);
    assert!(session.branch("main", &context).await.expect("branch").is_some());
    session.close(&context).await;
}
