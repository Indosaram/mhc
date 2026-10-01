//! Port of senpi packages/agent/test/harness/mutation-line.test.ts.
//!
//! The TS suite drives the line with deferred promises. Rust uses counted semaphores for the same
//! release points, so a release raised before the job parks is never lost and no test waits on a
//! scheduling accident.

mod support;

use std::sync::{Arc, Mutex};

use maho_agent::harness::session::session::{SessionError, SessionErrorKind};
use maho_agent::harness::session::MutationLine;
use tokio::sync::Semaphore;

/// Poll `condition` until it holds, bounded by a wall-clock deadline (never a fixed sleep).
async fn wait_until<F: Fn() -> bool>(condition: F) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "condition was never satisfied");
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn serializes_every_session_mutation() {
    let line = MutationLine::new();
    let gate = Arc::new(Semaphore::new(0));
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let first = {
        let line = line.clone();
        let gate = gate.clone();
        let order = order.clone();
        tokio::spawn(async move {
            line.run(|| async {
                order.lock().expect("order").push("first:start".to_owned());
                if let Ok(permit) = gate.acquire().await {
                    permit.forget();
                }
                order.lock().expect("order").push("first:end".to_owned());
                Ok::<_, SessionError>("first")
            })
            .await
        })
    };
    let second = {
        let line = line.clone();
        let order = order.clone();
        tokio::spawn(async move {
            line.run(|| async {
                order.lock().expect("order").push("second".to_owned());
                Ok::<_, SessionError>("second")
            })
            .await
        })
    };

    let order_for_wait = order.clone();
    wait_until(move || !order_for_wait.lock().expect("order").is_empty()).await;
    assert_eq!(order.lock().expect("order").clone(), vec!["first:start".to_owned()]);
    gate.add_permits(1);
    assert_eq!(first.await.expect("join").expect("first"), "first");
    assert_eq!(second.await.expect("join").expect("second"), "second");
    assert_eq!(
        order.lock().expect("order").clone(),
        vec!["first:start".to_owned(), "first:end".to_owned(), "second".to_owned()]
    );
}

#[tokio::test]
async fn continues_after_a_failed_job_while_preserving_the_original_failure() {
    let line = MutationLine::new();
    let failed = line
        .run(|| async { Err::<(), _>(SessionError::new(SessionErrorKind::Invariant, "mutation failed")) })
        .await
        .expect_err("failure");
    assert_eq!(failed.message, "mutation failed");
    assert_eq!(
        line.run(|| async { Ok::<_, SessionError>("next") }).await.expect("next"),
        "next"
    );
}

#[tokio::test]
async fn seals_queued_and_future_jobs_while_draining_the_running_job() {
    let line = MutationLine::new();
    let gate = Arc::new(Semaphore::new(0));
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let mut running = Box::pin(line.run({
        let gate = gate.clone();
        let order = order.clone();
        move || async move {
            order.lock().expect("order").push("running".to_owned());
            if let Ok(permit) = gate.acquire().await {
                permit.forget();
            }
            Ok::<_, SessionError>("running")
        }
    }));
    assert!(futures::poll!(running.as_mut()).is_pending());
    assert_eq!(order.lock().expect("order").clone(), vec!["running".to_owned()]);

    let mut queued = Box::pin(line.run(|| async { Ok::<_, SessionError>("queued") }));
    assert!(futures::poll!(queued.as_mut()).is_pending(), "a queued job waits for the line");

    let closed = SessionError::new(SessionErrorKind::Closed, "closed");
    let mut drain = Box::pin(line.seal(closed.clone()));
    assert!(futures::poll!(drain.as_mut()).is_pending(), "seal drains the running job");

    let late = line.run(|| async { Ok::<_, SessionError>("late") }).await.expect_err("late");
    assert_eq!(late.message, "closed");

    gate.add_permits(1);
    assert_eq!(running.await.expect("running"), "running");
    assert_eq!(queued.await.expect_err("queued").message, "closed");
    drain.await;
}
