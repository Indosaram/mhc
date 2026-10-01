//! Port of senpi packages/agent/test/harness/memory-session-repo.test.ts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::session::types::{SessionCreateOptions, SessionRepo, StorageBranchScan};
use maho_agent::harness::session::{MemorySessionRepo, MemorySessionRepoOptions};

const NOW: i64 = 1_700_000_000_000;

fn repo() -> MemorySessionRepo {
    MemorySessionRepo::new(MemorySessionRepoOptions {
        now: Some(Arc::new(|| NOW)),
    })
}

fn create_options(id: &str) -> SessionCreateOptions {
    SessionCreateOptions {
        id: Some(id.to_owned()),
        parent_session_id: None,
    }
}

/// `uuidTimestamp(id)`: the first 12 hex digits of the id carry the millisecond timestamp.
fn uuid_timestamp(id: &str) -> i64 {
    let compact: String = id.chars().filter(|character| *character != '-').collect();
    i64::from_str_radix(&compact[..12], 16).expect("uuid timestamp")
}

#[tokio::test]
async fn uses_its_injected_clock_for_generated_session_identity_and_metadata() {
    let repo = repo();
    let session = repo
        .create(SessionCreateOptions::default(), &BACKGROUND_CONTEXT)
        .await
        .expect("create");

    assert_eq!(session.metadata().created_at, NOW);
    assert_eq!(uuid_timestamp(&session.metadata().id), NOW);
    session.close(&BACKGROUND_CONTEXT).await;
    repo.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn returns_a_fresh_facade_after_close_while_retaining_one_session_and_storage() {
    let repo = repo();
    let context = BACKGROUND_CONTEXT.clone();
    let options = create_options("session");
    let first = repo.create(options, &context).await.expect("create");
    let first_branch = first.create_branch("main", None, &context).await.expect("branch");
    let admitted = first.set_name(Some("preserved".to_owned()), &context);

    let metadata = first.metadata().clone();
    let open_error = repo.open(metadata.clone(), &context).await.err().expect("already open");
    assert!(open_error.message.contains("already open"), "{}", open_error.message);

    admitted.await.expect("admitted write");
    first.close(&context).await;
    assert!(first.get_name(&BACKGROUND_CONTEXT).await.is_err());
    assert!(first
        .scan_branch(StorageBranchScan::new("entry"), &BACKGROUND_CONTEXT)
        .await
        .is_err());
    assert!(first_branch.get_tip_id(&BACKGROUND_CONTEXT).await.is_err());

    let second = repo.open(metadata, &BACKGROUND_CONTEXT).await.expect("reopen");
    assert_eq!(
        second.get_name(&BACKGROUND_CONTEXT).await.expect("name"),
        Some("preserved".to_owned())
    );
    second.close(&BACKGROUND_CONTEXT).await;
    repo.close(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn waits_for_an_explicit_mutation_before_closing_its_facade() {
    let repo = repo();
    let context = BACKGROUND_CONTEXT.clone();
    let options = create_options("session");
    let session = repo.create(options, &context).await.expect("create");
    let mutation = session.begin_mutation(&context).await.expect("mutation");
    let closed = Arc::new(AtomicBool::new(false));

    let mut closing = Box::pin(session.close(&context));
    assert!(
        futures::poll!(closing.as_mut()).is_pending(),
        "close must wait for the explicit mutation"
    );
    assert!(!closed.load(Ordering::SeqCst));

    mutation.end(&BACKGROUND_CONTEXT).await;
    closing.await;
    closed.store(true, Ordering::SeqCst);
    assert!(closed.load(Ordering::SeqCst));

    let metadata = session.metadata().clone();
    let reopened = repo.open(metadata, &context).await.expect("reopen");
    reopened.close(&context).await;
    repo.close(&context).await;
}

#[tokio::test]
async fn rejects_an_explicit_scope_that_had_not_acquired_before_facade_close() {
    let repo = repo();
    let context = BACKGROUND_CONTEXT.clone();
    let options = create_options("session");
    let session = repo.create(options, &context).await.expect("create");
    let first = session.begin_mutation(&context).await.expect("first");
    let mut second = Box::pin(session.begin_mutation(&context));
    assert!(
        futures::poll!(second.as_mut()).is_pending(),
        "the queued scope waits behind the first"
    );

    let mut closing = Box::pin(session.close(&context));
    assert!(
        futures::poll!(closing.as_mut()).is_pending(),
        "close must wait for the queued scope"
    );
    first.end(&context).await;
    let second_error = second.await.err().expect("second mutation");
    assert_eq!(second_error.message, "Session is closed");
    closing.await;

    let metadata = session.metadata().clone();
    let reopened = repo.open(metadata, &context).await.expect("reopen");
    reopened.close(&context).await;
    repo.close(&context).await;
}
