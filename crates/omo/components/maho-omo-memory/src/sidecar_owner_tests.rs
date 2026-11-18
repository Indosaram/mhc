//! Regressions for sidecar ownership: the two dedup sets, the own-before-sweep order, the shutdown
//! release, and the two offloaded tasks settling in either order. The offload seam is a queue the
//! test drains in whatever order it chooses, so no case sleeps or runs a thread.

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

    /// Takes every queued task, in the order the seam was called.
    fn take(&self) -> Vec<BlockingWork> {
        std::mem::take(
            &mut *self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn drain(&self) {
        run_in_order(self.take());
    }
}

/// Runs offloaded tasks in the order given, on this thread.
fn run_in_order(tasks: Vec<BlockingWork>) {
    for task in tasks {
        task();
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

#[test]
fn a_shutdown_that_beats_the_acquire_task_leaves_no_owner_lock() {
    let fixture = Fixture::new();
    fixture
        .owner
        .own(&fixture.deps(), "s-beat", &fixture.recall, &fixture.locks);
    fixture.owner.release_and_sweep(&fixture.deps(), "s-beat");

    let mut tasks = fixture.offload.take();
    assert_eq!(tasks.len(), 3, "acquire, first sweep, and the release");
    let release = tasks.remove(2);
    let acquire = tasks.remove(0);
    let first_sweep = tasks.remove(0);
    // The order that leaks: the claim ends before the acquire it depends on has even started.
    release();
    acquire();
    first_sweep();

    let lock_path = fixture.lock_path("s-beat");
    assert!(
        !lock_path.exists(),
        "an acquire settling after the claim ended must not publish a lock nobody can release"
    );
    assert!(!fixture.owner.owns("s-beat"));
    assert!(fixture.owner.in_flight().is_empty());
    assert!(
        fixture.warnings().is_empty(),
        "a shutdown that outran its own acquire is not a failure: {:?}",
        fixture.warnings()
    );
}

#[test]
fn an_acquire_whose_claim_already_ended_never_touches_the_locks_directory() {
    let root = tempfile::tempdir().unwrap();
    let recall = root.path().join("recall");
    std::fs::create_dir_all(crate::sidecar_sweep::sidecars_dir(&recall)).unwrap();
    // A FILE where the lock directory's parent belongs. Every publish fails loudly, so a silent pass
    // proves the acquire returned before it ever built a record for a claim that had already ended.
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let locks = blocked.join("locks");

    let owner = SidecarOwner::new();
    let offline = Arc::new(ManualOffload::default());
    let seam: Box<dyn Fn(BlockingWork) + Send + Sync> = {
        let offline = Arc::clone(&offline);
        Box::new(move |task: BlockingWork| offline.push(task))
    };
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let warn: Arc<dyn Fn(&str) + Send + Sync> = {
        let warnings = Arc::clone(&warnings);
        Arc::new(move |message: &str| {
            warnings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(message.to_string());
        })
    };
    let now_ms: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| NOW);
    let deps = OwnerDeps {
        offload: &seam,
        warn: &warn,
        now_ms: &now_ms,
    };

    owner.own(&deps, "s-ended", &recall, &locks);
    owner.release_and_sweep(&deps, "s-ended");
    let mut tasks = offline.take();
    assert_eq!(
        tasks.len(),
        3,
        "acquire, first sweep, and the release are all queued"
    );
    let release = tasks.remove(2);
    let acquire = tasks.remove(0);
    release();
    acquire();
    run_in_order(tasks);

    let warnings = warnings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        warnings.is_empty(),
        "an acquire whose claim already ended must not try to publish: {warnings:?}"
    );
    assert!(owner.in_flight().is_empty());
}

#[test]
fn a_publication_that_settles_after_the_claim_ended_is_released_by_the_acquire() {
    let fixture = Fixture::new();
    let lock_path = fixture.lock_path("s-late");
    let record = create_lock_record(
        LOCK_PURPOSE,
        CreateLockRecordOptions {
            run_id: Some("s-late".to_string()),
        },
    )
    .unwrap();
    acquire_lock(
        &lock_path,
        &record,
        &AcquireLockOptions {
            wait_timeout_ms: Some(0),
            retry_delay_ms: None,
            cancellation: None,
        },
    )
    .unwrap();
    assert!(lock_path.exists());

    let hold = Mutex::new(HoldState::default());
    assert!(
        end_hold(&hold).is_none(),
        "the release ran while the acquire was still sleeping, so it had nothing to release"
    );
    let late = settle_publication(&hold, record)
        .expect("a publication into an ended claim is handed back to the acquire");
    let quiet = |_: &str| {};
    release_record(&lock_path, &late, &quiet);

    assert!(
        !lock_path.exists(),
        "the acquire undoes its own late publication, so the directory is not locked forever"
    );
}

#[test]
fn a_release_after_the_acquire_published_takes_the_record_exactly_once() {
    let hold = Mutex::new(HoldState::default());
    let record = create_lock_record(
        LOCK_PURPOSE,
        CreateLockRecordOptions {
            run_id: Some("s-once".to_string()),
        },
    )
    .unwrap();

    assert!(
        settle_publication(&hold, record.clone()).is_none(),
        "a publication into a live claim is kept for the release"
    );
    assert_eq!(end_hold(&hold), Some(record));
    assert!(
        end_hold(&hold).is_none(),
        "a second release has nothing left to release"
    );
}

#[test]
fn a_second_own_of_one_session_keeps_the_first_claim() {
    let fixture = Fixture::new();
    let dir = fixture.expired_sidecar("s-dup");
    fixture
        .owner
        .own(&fixture.deps(), "s-dup", &fixture.recall, &fixture.locks);
    let first = fixture.offload.take();
    assert_eq!(
        first.len(),
        2,
        "one acquire and one sweep for the first claim"
    );

    fixture
        .owner
        .own(&fixture.deps(), "s-dup", &fixture.recall, &fixture.locks);

    assert_eq!(
        fixture.offload.queued(),
        0,
        "the duplicate schedules nothing: replacing the entry would drop the only record that can \
         release the lock already on disk"
    );
    assert!(fixture.owner.owns("s-dup"));

    run_in_order(first);
    let lock_path = fixture.lock_path("s-dup");
    assert!(lock_path.exists());

    fixture.owner.release_and_sweep(&fixture.deps(), "s-dup");
    run_in_order(fixture.offload.take());

    assert!(!fixture.owner.owns("s-dup"));
    assert!(
        !lock_path.exists(),
        "the surviving claim still holds the nonce its acquire wrote"
    );
    assert!(!dir.exists(), "the release still sweeps after the lock");
}

#[test]
fn a_release_never_removes_a_lock_written_by_another_owner() {
    let fixture = Fixture::new();
    fixture
        .owner
        .own(&fixture.deps(), "s-nonce", &fixture.recall, &fixture.locks);
    run_in_order(fixture.offload.take());
    let lock_path = fixture.lock_path("s-nonce");
    assert!(lock_path.exists());
    let foreign = create_lock_record(
        LOCK_PURPOSE,
        CreateLockRecordOptions {
            run_id: Some("foreign".to_string()),
        },
    )
    .unwrap();
    let foreign_json = serde_json::to_string(&foreign).unwrap();
    std::fs::write(&lock_path, &foreign_json).unwrap();

    fixture.owner.release_and_sweep(&fixture.deps(), "s-nonce");
    run_in_order(fixture.offload.take());

    assert!(
        lock_path.exists(),
        "the release verifies the nonce it acquired instead of deleting whatever is at the path"
    );
    assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), foreign_json);
}

/// A registry whose offload seam and warning sink reenter the registry, exactly as the wiring's
/// callbacks may. Both run on the caller's thread, so a registry lock held across either deadlocks
/// here instead of on a real pool.
struct ReentrantFixture {
    _root: tempfile::TempDir,
    recall: PathBuf,
    locks: PathBuf,
    owner: Arc<SidecarOwner>,
    offline: Arc<ManualOffload>,
    seam: Box<dyn Fn(BlockingWork) + Send + Sync>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    warnings: Arc<Mutex<Vec<String>>>,
    held: Arc<Mutex<Vec<&'static str>>>,
}

impl ReentrantFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let recall = root.path().join("recall");
        let locks = root.path().join("locks");
        std::fs::create_dir_all(crate::sidecar_sweep::sidecars_dir(&recall)).unwrap();
        std::fs::create_dir_all(&locks).unwrap();

        let owner = SidecarOwner::new();
        let registry: Arc<Mutex<Option<Arc<SidecarOwner>>>> = Arc::new(Mutex::new(None));
        *registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&owner));
        let offline = Arc::new(ManualOffload::default());
        let held = Arc::new(Mutex::new(Vec::new()));
        let seam: Box<dyn Fn(BlockingWork) + Send + Sync> = {
            let offline = Arc::clone(&offline);
            let registry = Arc::clone(&registry);
            let held = Arc::clone(&held);
            Box::new(move |task: BlockingWork| {
                let owner = registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .expect("the registry exists before it schedules");
                probe_guards(&owner, &held);
                offline.push(task);
            })
        };
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warn: Arc<dyn Fn(&str) + Send + Sync> = {
            let warnings = Arc::clone(&warnings);
            let registry = Arc::clone(&registry);
            let held = Arc::clone(&held);
            Arc::new(move |message: &str| {
                let owner = registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .expect("the registry exists before it warns");
                probe_guards(&owner, &held);
                warnings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(message.to_string());
            })
        };
        let now_ms: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| NOW);
        Self {
            _root: root,
            recall,
            locks,
            owner,
            offline,
            seam,
            warn,
            now_ms,
            warnings,
            held,
        }
    }

    fn deps(&self) -> OwnerDeps<'_> {
        OwnerDeps {
            offload: &self.seam,
            warn: &self.warn,
            now_ms: &self.now_ms,
        }
    }

    fn lock_path(&self) -> PathBuf {
        crate::sidecar_sweep::sidecar_owner_lock(
            &self.locks,
            &crate::sidecar_sweep::encode_session("s-reenter"),
        )
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn held(&self) -> Vec<&'static str> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Records every registry guard a caller-supplied callback finds held. `try_lock` rather than a read:
/// a reentrant `std::sync::Mutex` deadlocks, and a hung test reports nothing.
fn probe_guards(owner: &SidecarOwner, held: &Mutex<Vec<&'static str>>) {
    let mut probes = Vec::new();
    if owner.owned.try_lock().is_err() {
        probes.push("owned");
    }
    if owner.kick_once.try_lock().is_err() {
        probes.push("kick_once");
    }
    if owner.in_flight.try_lock().is_err() {
        probes.push("in_flight");
    }
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(probes);
}

#[test]
fn the_offload_seam_runs_without_a_held_registry_lock() {
    let fixture = ReentrantFixture::new();

    fixture.owner.own(
        &fixture.deps(),
        "s-reenter",
        &fixture.recall,
        &fixture.locks,
    );
    assert!(
        fixture.owner.owns("s-reenter"),
        "the claim is published before the seam is called"
    );
    run_in_order(fixture.offline.take());
    assert!(
        fixture.lock_path().exists(),
        "the acquire ran through the reentrant seam and published"
    );
    assert!(
        fixture.held().is_empty(),
        "the seam must never be called with a registry guard held: {:?}",
        fixture.held()
    );

    fixture
        .owner
        .release_and_sweep(&fixture.deps(), "s-reenter");
    run_in_order(fixture.offline.take());
    assert!(!fixture.owner.owns("s-reenter"));
    assert!(!fixture.lock_path().exists());
    assert!(fixture.warnings().is_empty());
}

#[test]
fn the_warn_sink_runs_without_a_held_registry_lock() {
    let fixture = ReentrantFixture::new();
    fixture.owner.own(
        &fixture.deps(),
        "s-reenter",
        &fixture.recall,
        &fixture.locks,
    );
    run_in_order(fixture.offline.take());
    assert!(fixture.lock_path().exists());
    // A file where the sweep's sidecar tree belongs makes the listing fail, which is one portable way
    // to reach the warning sink from inside the release task.
    let sidecars = crate::sidecar_sweep::sidecars_dir(&fixture.recall);
    std::fs::remove_dir(&sidecars).unwrap();
    std::fs::write(&sidecars, b"not a directory").unwrap();

    fixture
        .owner
        .release_and_sweep(&fixture.deps(), "s-reenter");
    run_in_order(fixture.offline.take());

    let warnings = fixture.warnings();
    assert_eq!(
        warnings.len(),
        1,
        "the failed listing is reported exactly once: {warnings:?}"
    );
    assert!(
        warnings[0].contains("sweep listing failed"),
        "the sink reentered the registry while the release reported: {warnings:?}"
    );
    assert!(
        !fixture.lock_path().exists(),
        "the release still ran before the sweep that warned"
    );
    assert!(
        fixture.held().is_empty(),
        "the warning sink must never be called with a registry guard held: {:?}",
        fixture.held()
    );
}

#[test]
fn a_claim_from_inside_the_seam_keeps_the_kick_once_set_free() {
    let fixture = ReentrantFixture::new();
    let elsewhere = fixture.recall.with_file_name("elsewhere");
    std::fs::create_dir_all(crate::sidecar_sweep::sidecars_dir(&elsewhere)).unwrap();
    let nested = Arc::new(ManualOffload::default());
    let quiet: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| NOW);
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let held = Arc::new(Mutex::new(Vec::new()));
    let reentrant: Box<dyn Fn(BlockingWork) + Send + Sync> = {
        let owner = Arc::clone(&fixture.owner);
        let offline = Arc::clone(&fixture.offline);
        let nested = Arc::clone(&nested);
        let seen = Arc::clone(&seen);
        let held = Arc::clone(&held);
        let elsewhere = elsewhere.clone();
        let locks = fixture.locks.clone();
        Box::new(move |task: BlockingWork| {
            probe_guards(&owner, &held);
            // Only claim from inside the seam when the probe found the guards free: a nested claim
            // under a held `kick_once` would block instead of reporting.
            if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1
                && held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
            {
                let push = |task: BlockingWork| nested.push(task);
                owner.own(
                    &OwnerDeps {
                        offload: &push,
                        warn: &quiet,
                        now_ms: &clock,
                    },
                    "s-elsewhere",
                    &elsewhere,
                    &locks,
                );
            }
            offline.push(task);
        })
    };
    let deps = OwnerDeps {
        offload: &reentrant,
        warn: &fixture.warn,
        now_ms: &fixture.now_ms,
    };

    fixture
        .owner
        .own(&deps, "s-reenter", &fixture.recall, &fixture.locks);

    assert!(
        held.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "the sweep kick must not reach the seam with `kick_once` held"
    );
    assert!(
        fixture.owner.owns("s-elsewhere"),
        "a claim made from inside the sweep kick must reach the owned map"
    );
    run_in_order(nested.take());
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
