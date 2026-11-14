//! Regressions for sidecar ownership: the two dedup sets, the own-before-sweep order, and the
//! shutdown release. The offload seam is a queue the test drains, so no case sleeps.

use std::path::PathBuf;
use std::sync::Mutex;

use super::*;

const NOW: i64 = 1_800_000_000_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Default)]
struct ManualOffload {
    queue: Mutex<Vec<BlockingWork>>,
}

impl ManualOffload {
    fn push(&self, task: BlockingWork) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(task);
    }

    fn queued(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn drain(&self) {
        loop {
            let next = {
                self.queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop()
            };
            match next {
                Some(task) => task(),
                None => break,
            }
        }
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    recall: PathBuf,
    locks: PathBuf,
    offload: Arc<ManualOffload>,
    offload_seam: Box<dyn Fn(BlockingWork) + Send + Sync>,
    warnings: Arc<Mutex<Vec<String>>>,
    owner: Arc<SidecarOwner>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let recall = root.path().join("recall");
        let locks = root.path().join("locks");
        std::fs::create_dir_all(crate::sidecar_sweep::sidecars_dir(&recall)).unwrap();
        std::fs::create_dir_all(&locks).unwrap();
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warn: Arc<dyn Fn(&str) + Send + Sync> = {
            let warnings = Arc::clone(&warnings);
            Arc::new(move |message: &str| {
                warnings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(message.to_string())
            })
        };
        let now_ms: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| NOW);
        let owner = SidecarOwner::new();
        let offload = Arc::new(ManualOffload::default());
        let offload_seam = {
            let offload = Arc::clone(&offload);
            Box::new(move |task: BlockingWork| offload.push(task))
                as Box<dyn Fn(BlockingWork) + Send + Sync>
        };
        Self {
            _root: root,
            recall,
            locks,
            offload,
            offload_seam,
            warnings,
            owner,
            warn,
            now_ms,
        }
    }

    fn deps(&self) -> OwnerDeps<'_> {
        OwnerDeps {
            offload: &self.offload_seam,
            warn: &self.warn,
            now_ms: &self.now_ms,
        }
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn expired_sidecar(&self, session: &str) -> PathBuf {
        let dir = crate::sidecar_sweep::sidecar_dir(&self.recall, session);
        std::fs::create_dir_all(&dir).unwrap();
        let target = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(u64::try_from(NOW - 30 * DAY_MS).unwrap());
        filetime::set_file_mtime(&dir, filetime::FileTime::from_system_time(target)).unwrap();
        dir
    }

    fn lock_path(&self, session: &str) -> PathBuf {
        crate::sidecar_sweep::sidecar_owner_lock(
            &self.locks,
            &crate::sidecar_sweep::encode_session(session),
        )
    }
}

#[test]
fn own_records_ownership_before_scheduling_the_acquire_and_sweep() {
    let fixture = Fixture::new();

    fixture
        .owner
        .own(&fixture.deps(), "s1", &fixture.recall, &fixture.locks);

    assert!(
        fixture.owner.owns("s1"),
        "ownership is visible synchronously, before any blocking work runs"
    );
    assert_eq!(
        fixture.offload.queued(),
        2,
        "one owner acquire and one sweep are queued, never run inline"
    );
}

#[test]
fn a_resumed_expired_session_survives_its_own_first_sweep() {
    let fixture = Fixture::new();
    let dir = fixture.expired_sidecar("s-resume");

    fixture
        .owner
        .own(&fixture.deps(), "s-resume", &fixture.recall, &fixture.locks);
    fixture.offload.drain();

    assert!(
        dir.exists(),
        "the owned set is built before the sweep runs, so its own directory survives"
    );
    assert!(
        fixture.owner.in_flight().is_empty(),
        "draining the seam finishes the sweep and clears its in-flight mark"
    );
}

#[test]
fn a_second_session_on_the_same_recall_dir_does_not_resweep() {
    let fixture = Fixture::new();

    fixture
        .owner
        .own(&fixture.deps(), "s1", &fixture.recall, &fixture.locks);
    assert_eq!(fixture.offload.queued(), 2);
    fixture
        .owner
        .own(&fixture.deps(), "s2", &fixture.recall, &fixture.locks);

    assert_eq!(
        fixture.offload.queued(),
        3,
        "the second own acquires its own lock but does not kick a second sweep"
    );
}

#[test]
fn shutdown_releases_the_owner_lock_then_sweeps_and_the_dir_goes() {
    let fixture = Fixture::new();
    let dir = fixture.expired_sidecar("s-gone");
    fixture
        .owner
        .own(&fixture.deps(), "s-gone", &fixture.recall, &fixture.locks);
    fixture.offload.drain();
    let lock_path = fixture.lock_path("s-gone");
    assert!(lock_path.exists(), "the owner lock is held for the session");

    fixture.owner.release_and_sweep(&fixture.deps(), "s-gone");
    fixture.offload.drain();

    assert!(!fixture.owner.owns("s-gone"));
    assert!(
        !lock_path.exists(),
        "the release must precede the sweep, or the sweep could not claim"
    );
    assert!(
        !dir.exists(),
        "the sweep after shutdown reclaims the session's own expired directory"
    );
}

#[test]
fn an_unknown_session_releases_nothing() {
    let fixture = Fixture::new();

    fixture
        .owner
        .release_and_sweep(&fixture.deps(), "never-owned");

    assert_eq!(fixture.offload.queued(), 0);
    assert!(fixture.warnings().is_empty());
}

#[test]
fn an_inert_owner_records_ownership_without_taking_a_lock() {
    let fixture = Fixture::new();
    let owner = SidecarOwner::inert();

    owner.own(&fixture.deps(), "s1", &fixture.recall, &fixture.locks);

    assert!(owner.owns("s1"));
    assert!(
        !fixture.lock_path("s1").exists(),
        "the inert registry only records the bookkeeping"
    );
}

#[tokio::test]
async fn when_idle_resolves_immediately_when_nothing_is_in_flight() {
    let fixture = Fixture::new();

    fixture.owner.when_idle().await.unwrap();
}

#[tokio::test]
async fn when_idle_waits_for_the_running_sweep_and_wakes_without_polling() {
    let fixture = Fixture::new();
    let expired = fixture.expired_sidecar("dead");
    fixture
        .owner
        .own(&fixture.deps(), "s1", &fixture.recall, &fixture.locks);
    assert_eq!(
        fixture.owner.in_flight().len(),
        1,
        "the ownership kick scheduled a sweep, so the drain point is occupied"
    );

    // Registered BEFORE the sweep runs: the waiter must be woken by the completion, not by a poll.
    let idle = fixture.owner.when_idle();
    tokio::pin!(idle);
    assert!(
        futures_poll_once(&mut idle).await.is_pending(),
        "it must not resolve while the sweep is still queued"
    );

    fixture.offload.drain();

    idle.await.unwrap();
    assert!(fixture.owner.in_flight().is_empty());
    assert!(!expired.exists(), "the sweep it waited for did run");
}

/// Polls a future exactly once and reports whether it is still pending.
async fn futures_poll_once<F: std::future::Future>(
    future: &mut std::pin::Pin<&mut F>,
) -> std::task::Poll<F::Output> {
    std::future::poll_fn(|context| std::task::Poll::Ready(future.as_mut().poll(context))).await
}
