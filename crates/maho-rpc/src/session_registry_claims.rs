//! senpi `modes/rpc/session-registry-claims.ts`: the registry's path claims. The in-process reservation set and
//! the cross-generation claim (`host_reservations.rs`) follow whatever file each entry's runtime is actually
//! writing.
//!
//! NATIVE-DIVERGENCE(senpi `pathReservations.claim` is async): the native claim store writes its claim file
//! synchronously and reports a failed write through its bound reporter instead of rejecting, so
//! `take_cross_generation_claim` returns at once; the pending-claim window it opens is the same.
use crate::host_reservations::{BoundPathReservations, SESSION_PATH_RETRY_AFTER_MS};
use crate::session_registry::RpcSessionRegistryError;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

/// One open between its in-process reservation and its entry: its cross-generation claim is in flight.
#[derive(Clone)]
pub struct PendingClaim {
    id: u64,
    settled: tokio::sync::watch::Receiver<bool>,
}

/// Opens between their in-process reservation and their entry, by path (senpi `PendingClaims`).
pub type PendingClaims = Arc<Mutex<HashMap<String, PendingClaim>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Marks `session_path` as mid-claim (senpi `beginPendingClaim`). The returned settle is called once the open has
/// either recorded its entry or given the reservation back, so a waiter that wakes always finds one or the other.
pub fn begin_pending_claim(
    pending: &PendingClaims,
    session_path: &str,
) -> impl FnOnce() + Send + 'static {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (settle, settled) = tokio::sync::watch::channel(false);
    lock(pending).insert(session_path.to_owned(), PendingClaim { id, settled });
    let (pending, session_path) = (Arc::clone(pending), session_path.to_owned());
    move || {
        let mut pending = lock(&pending);
        if pending
            .get(&session_path)
            .is_some_and(|claim| claim.id == id)
        {
            pending.remove(&session_path);
        }
        drop(pending);
        let _ = settle.send(true);
    }
}

/// Takes the cross-generation claim for an open that already holds the in-process reservation, and returns the
/// settle `begin_pending_claim` handed out: the caller settles it once its entry is recorded (senpi
/// `takeCrossGenerationClaim`). A live claim another generation holds gives the reservation back and refuses
/// with the owner.
pub fn take_cross_generation_claim(
    reservations: &mut HashSet<String>,
    pending: &PendingClaims,
    path_reservations: &mut BoundPathReservations,
    session_path: &str,
) -> Result<impl FnOnce() + Send + 'static, RpcSessionRegistryError> {
    let settle = begin_pending_claim(pending, session_path);
    let Some(holder) = path_reservations.claim(session_path, true) else {
        return Ok(settle);
    };
    reservations.remove(session_path);
    settle();
    Err(RpcSessionRegistryError::new(
        "session_path_in_use",
        None,
        Some(serde_json::json!({
            "owner": holder,
            "retry_after_ms": SESSION_PATH_RETRY_AFTER_MS,
        })),
    ))
}

/// Waits out another open of this path that is still taking its claim (senpi#3485, `settlePendingClaim`). Without
/// the wait the path looked reserved by no session at all and a reopen was refused for a session about to be
/// served. Bounded by the close grace window: a claim that never settles (a wedged daemon directory) ends this
/// open with a retryable `session_path_in_use { reason: "claim_in_progress" }`.
pub async fn settle_pending_claim(
    pending: &PendingClaims,
    session_path: &str,
    grace_ms: u64,
) -> Result<(), RpcSessionRegistryError> {
    let late = tokio::time::Instant::now() + std::time::Duration::from_millis(grace_ms);
    loop {
        let Some(claim) = lock(pending).get(session_path).cloned() else {
            return Ok(());
        };
        let mut settled = claim.settled;
        let wait = settled.wait_for(|settled| *settled);
        if tokio::time::timeout_at(late, wait).await.is_err() {
            return Err(RpcSessionRegistryError::new(
                "session_path_in_use",
                Some("another open of this path is still taking its claim"),
                Some(serde_json::json!({
                    "reason": "claim_in_progress",
                    "retry_after_ms": SESSION_PATH_RETRY_AFTER_MS,
                })),
            ));
        }
        // A settled claim is gone from the map unless a newer open of the path took its place.
        if lock(pending)
            .get(session_path)
            .is_some_and(|current| current.id == claim.id)
        {
            return Ok(());
        }
    }
}

/// One entry's path and durable identity as its live runtime reports them (the fields `syncRuntimeMetadata`
/// reconciles).
pub struct RuntimeMetadata<'a> {
    pub reservation_key: &'a mut Option<String>,
    pub session_path: &'a mut Option<String>,
    pub durable_session_id: &'a mut Option<String>,
    pub attachments: i64,
}

/// Reconcile path and durable identity after runtime replacement (senpi `syncRuntimeMetadata`, per entry):
/// `current_path` and `current_id` are what the runtime's manager is writing now.
///
/// A moved path is not the only reason to reconcile: a session opened WITHOUT `sessionPath` lands its created
/// file straight into `session_path` and never takes a reservation, so comparing paths alone leaves that file
/// unclaimed forever and a later open of it builds a SECOND runtime over the same transcript. Reconcile whenever
/// the canonical key held is not the key the runtime is actually writing. The originally canonicalized key is
/// kept while the runtime still points at the same path.
pub fn sync_runtime_metadata(
    entry: RuntimeMetadata<'_>,
    current_path: Option<String>,
    current_id: String,
    reservations: &mut HashSet<String>,
    mut path_reservations: Option<&mut BoundPathReservations>,
) {
    let current_key = current_path.as_deref().map(|path| {
        crate::session_path_key::canonical_session_path(std::path::Path::new(path))
            .to_string_lossy()
            .into_owned()
    });
    if current_path != *entry.session_path || current_key != *entry.reservation_key {
        if let Some(key) = entry.reservation_key.as_deref() {
            reservations.remove(key);
            if let Some(claims) = path_reservations.as_deref_mut() {
                claims.release(key);
            }
        }
        if let Some(key) = &current_key {
            reservations.insert(key.clone());
            // A replacement moved this session to another file; the claim follows it, carrying the attachment
            // state it is now held at. A file a live foreign generation holds is left alone by claim() itself.
            if let Some(claims) = path_reservations {
                claims.claim(key, entry.attachments > 0);
            }
        }
        *entry.reservation_key = current_key;
        *entry.session_path = current_path;
    }
    *entry.durable_session_id = Some(current_id);
}

/// Waits out a teardown already in flight for this path before the open decides (senpi
/// `settleClosingReservation`). A close frees the path, but the entry keeps its reservation until its runtime is
/// disposed, so an open landing inside that window waits for the teardown instead of being refused; bounded by
/// the grace window that bounds the teardown itself, so a wedged disposal still ends in the ordinary refusal.
pub async fn settle_closing_reservation(
    close_completion: Option<impl std::future::Future<Output = ()>>,
    close_grace_ms: u64,
) {
    if let Some(completion) = close_completion {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(close_grace_ms), completion)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(dir: &std::path::Path, pid: u32) -> BoundPathReservations {
        crate::host_reservations::create_session_path_reservations(
            dir,
            format!("instance-{pid}"),
            Some(pid),
            |_| {},
        )
    }

    #[tokio::test]
    async fn a_settled_claim_wakes_its_waiter_and_leaves_the_map() {
        let pending = PendingClaims::default();
        let settle = begin_pending_claim(&pending, "/p");
        let waiter = settle_pending_claim(&pending, "/p", 1_000);
        let (waited, ()) = tokio::join!(waiter, async { settle() });
        assert_eq!(waited, Ok(()));
        assert!(lock(&pending).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_claim_that_never_settles_ends_the_wait_with_a_retryable_refusal() {
        let pending = PendingClaims::default();
        let _settle = begin_pending_claim(&pending, "/p");
        let refused = settle_pending_claim(&pending, "/p", 50).await.unwrap_err();
        assert_eq!(refused.code, "session_path_in_use");
        let detail = refused.detail.expect("detail");
        assert_eq!(detail["reason"], "claim_in_progress");
        assert_eq!(detail["retry_after_ms"], SESSION_PATH_RETRY_AFTER_MS);
    }

    #[tokio::test]
    async fn an_unclaimed_path_needs_no_wait() {
        assert_eq!(
            settle_pending_claim(&PendingClaims::default(), "/p", 0).await,
            Ok(())
        );
    }

    #[test]
    fn a_stale_settle_never_removes_a_newer_claim_of_the_path() {
        let pending = PendingClaims::default();
        let first = begin_pending_claim(&pending, "/p");
        let _second = begin_pending_claim(&pending, "/p");
        first();
        assert!(lock(&pending).contains_key("/p"));
    }

    #[test]
    fn a_free_path_is_claimed_and_its_settle_handed_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut claims = claims(dir.path(), std::process::id());
        let pending = PendingClaims::default();
        let mut reservations = HashSet::from(["/p".to_owned()]);
        let settle = take_cross_generation_claim(&mut reservations, &pending, &mut claims, "/p")
            .expect("a free path is claimed");
        assert!(lock(&pending).contains_key("/p"));
        settle();
        assert!(lock(&pending).is_empty());
        assert!(reservations.contains("/p"));
    }

    #[cfg(unix)]
    #[test]
    fn a_live_foreign_claim_gives_the_reservation_back_and_names_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        // A live process (this one's parent) holds the path under another instance.
        let parent = std::os::unix::process::parent_id();
        claims(dir.path(), parent).claim("/p", true);
        let mut ours = claims(dir.path(), std::process::id());
        let pending = PendingClaims::default();
        let mut reservations = HashSet::from(["/p".to_owned()]);
        let refused = take_cross_generation_claim(&mut reservations, &pending, &mut ours, "/p")
            .err()
            .expect("a live foreign claim refuses");
        assert_eq!(refused.code, "session_path_in_use");
        let detail = refused.detail.expect("detail");
        assert_eq!(detail["owner"]["pid"], parent);
        assert_eq!(detail["retry_after_ms"], SESSION_PATH_RETRY_AFTER_MS);
        assert!(!reservations.contains("/p"));
        assert!(lock(&pending).is_empty());
    }

    // senpi#1850: a pathless session's created file is reserved once the runtime reports it.
    #[test]
    fn a_pathless_session_reserves_the_file_its_runtime_created() {
        let dir = tempfile::tempdir().unwrap();
        let created = dir
            .path()
            .join("created.jsonl")
            .to_string_lossy()
            .into_owned();
        let (mut key, mut path, mut id) = (None, None, None);
        let mut reservations = HashSet::new();
        sync_runtime_metadata(
            RuntimeMetadata {
                reservation_key: &mut key,
                session_path: &mut path,
                durable_session_id: &mut id,
                attachments: 1,
            },
            Some(created.clone()),
            "durable".into(),
            &mut reservations,
            None,
        );
        let canonical =
            crate::session_path_key::canonical_session_path(std::path::Path::new(&created))
                .to_string_lossy()
                .into_owned();
        assert_eq!(key.as_deref(), Some(canonical.as_str()));
        assert_eq!(path.as_deref(), Some(created.as_str()));
        assert_eq!(id.as_deref(), Some("durable"));
        assert!(reservations.contains(&canonical));
    }

    #[test]
    fn a_replacement_moves_the_reservation_to_the_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = |name: &str| {
            crate::session_path_key::canonical_session_path(&dir.path().join(name))
                .to_string_lossy()
                .into_owned()
        };
        let old = canonical("old.jsonl");
        let new = canonical("new.jsonl");
        let (mut key, mut path, mut id) = (Some(old.clone()), Some(old.clone()), None);
        let mut reservations = HashSet::from([old.clone()]);
        sync_runtime_metadata(
            RuntimeMetadata {
                reservation_key: &mut key,
                session_path: &mut path,
                durable_session_id: &mut id,
                attachments: 0,
            },
            Some(new.clone()),
            "next".into(),
            &mut reservations,
            None,
        );
        assert_eq!(reservations, HashSet::from([new.clone()]));
        assert_eq!(key, Some(new.clone()));
        assert_eq!(path, Some(new));
        // Unchanged spelling and key: nothing moves.
        let before = reservations.clone();
        let same_path = path.clone();
        sync_runtime_metadata(
            RuntimeMetadata {
                reservation_key: &mut key,
                session_path: &mut path,
                durable_session_id: &mut id,
                attachments: 0,
            },
            same_path,
            "next".into(),
            &mut reservations,
            None,
        );
        assert_eq!(reservations, before);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closing_reservation_is_waited_out_only_up_to_the_grace_window() {
        settle_closing_reservation(Some(std::future::pending::<()>()), 30).await;
        settle_closing_reservation(None::<std::future::Ready<()>>, 30).await;
        settle_closing_reservation(Some(std::future::ready(())), 30).await;
    }
}
