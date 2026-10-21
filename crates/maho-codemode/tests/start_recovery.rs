use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use maho_codemode::extension::start_recovery::{StartRecovery, StartSession};
use maho_ext_api::ExtensionFailure;
use serde_json::json;

fn recovery(start: StartSession<()>) -> Arc<StartRecovery<()>> {
    Arc::new(StartRecovery::new(start, Arc::new(|_| {})))
}

#[tokio::test]
async fn concurrent_evals_share_recreation_when_start_failed() {
    // Given a failed start and an event-gated recreation.
    let attempts = Arc::new(AtomicUsize::new(0));
    let (entered, mut entry) = tokio::sync::mpsc::channel(1);
    let (release, released) = tokio::sync::oneshot::channel();
    let released = Arc::new(Mutex::new(Some(released)));
    let start: StartSession<()> = Arc::new({
        let attempts = attempts.clone();
        move |event, ()| {
            assert_eq!(event, json!({"sessionId":"fixture"}));
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let entered = entered.clone();
            let released = if attempt == 0 {
                None
            } else {
                released.lock().unwrap().take()
            };
            Box::pin(async move {
                if attempt == 0 {
                    return Err(ExtensionFailure::new("initial failure"));
                }
                entered.send(()).await.unwrap();
                released.unwrap().await.unwrap();
                Ok(())
            })
        }
    });
    let recovery = recovery(start);
    assert!(
        recovery
            .run_start(json!({"sessionId":"fixture"}), ())
            .await
            .is_err()
    );
    // When two evals recover before recreation settles.
    let first = recovery.recover((), "fixture");
    let second = recovery.recover((), "fixture");
    let release_on_entry = async {
        entry.recv().await.unwrap();
        release.send(()).unwrap();
    };
    let (first, second, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(first, second, release_on_entry)
    })
    .await
    .unwrap();
    // Then both evals share the single successful recreation.
    assert_eq!((first.unwrap(), second.unwrap()), (true, true));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn recreation_failure_is_cached_until_next_start() {
    // Given an initial failure followed by a failed recreation.
    let attempts = Arc::new(AtomicUsize::new(0));
    let recovery = recovery(Arc::new({
        let attempts = attempts.clone();
        move |_, ()| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if attempt < 2 {
                    Err(ExtensionFailure::new(format!("failure-{attempt}")))
                } else {
                    Ok(())
                }
            })
        }
    }));
    assert!(recovery.run_start(json!(null), ()).await.is_err());
    let first = recovery.recover((), "fixture").await.unwrap_err();
    // When later evals ask to recover again.
    let next = recovery.recover((), "fixture").await.unwrap_err();
    // Then the same failure is returned without a third startup.
    assert_eq!(next, first);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    recovery.run_start(json!(null), ()).await.unwrap();
    assert!(!recovery.recover((), "fixture").await.unwrap());
}

#[tokio::test]
async fn ended_session_cannot_recover_failed_start() {
    // Given a failed startup.
    let recovery = recovery(Arc::new(|_, ()| {
        Box::pin(async { Err(ExtensionFailure::new("failed")) })
    }));
    assert!(recovery.run_start(json!(null), ()).await.is_err());
    // When the session ends.
    recovery.session_ended();
    // Then eval cannot recreate its runtime.
    assert!(!recovery.recover((), "fixture").await.unwrap());
}

#[tokio::test]
async fn ending_session_cancels_inflight_recreation() {
    // Given a recreation waiting on an exact startup event.
    let attempts = Arc::new(AtomicUsize::new(0));
    let (entered, mut entry) = tokio::sync::mpsc::channel(1);
    let recovery = recovery(Arc::new(move |_, ()| {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        let entered = entered.clone();
        Box::pin(async move {
            if attempt == 0 {
                return Err(ExtensionFailure::new("failed"));
            }
            entered.send(()).await.unwrap();
            std::future::pending().await
        })
    }));
    assert!(recovery.run_start(json!(null), ()).await.is_err());
    // When the session ends during recreation.
    let end_on_entry = async {
        entry.recv().await.unwrap();
        recovery.session_ended();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(recovery.recover((), "fixture"), end_on_entry)
    })
    .await
    .unwrap();
    // Then the shared attempt fails and remains ineligible after shutdown.
    assert!(result.is_err());
    assert!(!recovery.recover((), "fixture").await.unwrap());
}

#[tokio::test]
async fn recovery_is_ineligible_without_failed_start() {
    // Given a session that has never failed startup.
    let attempts = Arc::new(AtomicUsize::new(0));
    let recovery = recovery(Arc::new({
        let attempts = attempts.clone();
        move |_, ()| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }));
    // When eval requests recovery.
    let result = recovery.recover((), "fixture").await.unwrap();
    // Then no runtime is created.
    assert!(!result);
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
}
