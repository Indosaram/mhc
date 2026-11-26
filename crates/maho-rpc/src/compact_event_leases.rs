//! senpi `modes/rpc/compact-event-leases.ts`: the ordinary-event leases of one host (S13, R4.B6;
//! RULING-65 S13-Q1/Q2 = A). Records past F are staged once into a spillable store and leased to
//! peers at their accepted sequence until a consumer `release_recovery` receipt or peer end.

use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};
use crate::compact_read_lease::CompactReadLease;
use crate::compact_record_staging::{RecordContent, stage_record};
use crate::compact_recovery_bytes::{
    CancelSignal, FRAGMENT_MAX_BYTES, FrozenBytes, RecoveryBytesError, RecoveryPage, produce_page,
};
use crate::compact_recovery_ids::protocol_id;
use crate::compact_recovery_types::{
    EventLease, RecoveryCaller, RecoveryOutcome, RepairReason, StorageUnavailableReason, recovered,
    repair_needed, storage_unavailable,
};
use crate::compact_transfer_staging::{StagedContent, StagingOpError, TransferStaging};
use futures::future::Shared;
use futures::future::{BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// F (R4.A3): the largest compact physical record, its newline included; two D-byte fragments.
pub const COMPACT_RECORD_MAX_BYTES: usize = 2 * FRAGMENT_MAX_BYTES;
/// Live event leases one peer incarnation may hold (RULING-65 S13-Q1 (2)); its end frees every one.
pub const PEER_EVENT_LEASES_MAX: usize = 32;
/// Released lease ids a peer remembers so a retried receipt is a typed `duplicate`.
pub const RELEASED_LEASES_REMEMBERED: usize = 256;
/// The one component an event lease names: the accepted record's exact JSON text.
pub const EVENT_RECORD_COMPONENT: &str = "record";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLeaseRefusal {
    LeaseLimit,
    StorageUnavailable,
}

impl EventLeaseRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeaseLimit => "lease_limit",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseOutcome {
    Released,
    Held,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRefIdentity {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseReceipt {
    pub lease: LeaseRefIdentity,
    pub outcome: ReleaseOutcome,
    pub remaining: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLeaseCounters {
    pub open: usize,
    pub peers: usize,
    pub minted: u64,
    pub released: u64,
    pub duplicates: u64,
    pub abandoned: u64,
    pub content_bytes: usize,
    pub refused: BTreeMap<EventLeaseRefusal, u64>,
}

type SharedOpenFut = Shared<BoxFuture<'static, Result<Arc<dyn FrozenBytes>, RecoveryBytesError>>>;

struct SharedContent {
    key: Option<usize>,
    content: StagedContent,
    holders: usize,
    opened: Option<SharedOpenFut>,
}

pub struct HeldLease {
    pub lease: EventLease,
    pub session_id: String,
    shared: Arc<Mutex<SharedContent>>,
    admissions: Mutex<HashSet<String>>,
    reads: CompactReadLease,
    released_flag: AtomicBool,
}

impl HeldLease {
    pub fn add_admission(&self, admission_id: impl Into<String>) {
        self.admissions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(admission_id.into());
    }

    pub fn admissions_snapshot(&self) -> Vec<String> {
        self.admissions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn release_once(&self) -> bool {
        if self.released_flag.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.reads.release();
        true
    }
}

struct ReleasedRecord {
    session_id: String,
    admissions: HashSet<String>,
}

#[derive(Default)]
struct PeerLeases {
    held: HashMap<String, Arc<HeldLease>>,
    released: HashMap<String, ReleasedRecord>,
    released_order: VecDeque<String>,
}

pub type LeaseEndListener = Arc<dyn Fn(&str) + Send + Sync>;

struct EventLeasesState {
    peers: HashMap<String, PeerLeases>,
    shared: HashMap<usize, Arc<Mutex<SharedContent>>>,
    ended: Vec<LeaseEndListener>,
    refusals: BTreeMap<EventLeaseRefusal, u64>,
    staging: Option<TransferStaging>,
    open: usize,
    minted: u64,
    released: u64,
    duplicates: u64,
    abandoned: u64,
    content_bytes: usize,
}

#[derive(Clone)]
pub struct EventLeases {
    pub peer_limit: usize,
    inner: Arc<Mutex<EventLeasesState>>,
}

impl Default for EventLeases {
    fn default() -> Self {
        Self::new(PEER_EVENT_LEASES_MAX)
    }
}

impl EventLeases {
    pub fn new(peer_limit: usize) -> Self {
        Self {
            peer_limit,
            inner: Arc::new(Mutex::new(EventLeasesState {
                peers: HashMap::new(),
                shared: HashMap::new(),
                ended: Vec::new(),
                refusals: BTreeMap::new(),
                staging: None,
                open: 0,
                minted: 0,
                released: 0,
                duplicates: 0,
                abandoned: 0,
                content_bytes: 0,
            })),
        }
    }

    pub fn on_lease_end(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ended
            .push(Arc::new(listener));
    }

    /// Mints an event lease for one peer admission, optionally sharing staged content across peers
    /// via `shared_key` (e.g. pointer identity of the accepted record).
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &self,
        socket_epoch: &str,
        session_id: &str,
        admission_id: &str,
        session_epoch: &str,
        sequence: u64,
        record: &Value,
        content: RecordContent,
        shared_key: Option<usize>,
    ) -> Result<EventLease, EventLeaseRefusal> {
        let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let peer_count = st.peers.get(socket_epoch).map_or(0, |p| p.held.len());
        if peer_count >= self.peer_limit {
            *st.refusals
                .entry(EventLeaseRefusal::LeaseLimit)
                .or_insert(0) += 1;
            return Err(EventLeaseRefusal::LeaseLimit);
        }

        let shared = match Self::stage_in(&mut st, record, content, shared_key) {
            Ok(s) => s,
            Err(()) => {
                *st.refusals
                    .entry(EventLeaseRefusal::StorageUnavailable)
                    .or_insert(0) += 1;
                return Err(EventLeaseRefusal::StorageUnavailable);
            }
        };

        let (entry_len, entry_digest) = {
            let mut s_guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
            s_guard.holders += 1;
            let entry = s_guard.content.ref_.entry();
            (entry.length, entry.digest.as_str().to_owned())
        };

        st.minted += 1;
        let id = protocol_id(&format!("event:{socket_epoch}:{}", st.minted)).into_inner();
        let lease = EventLease::new(
            id.clone(),
            admission_id.to_owned(),
            session_epoch.to_owned(),
            sequence,
            EVENT_RECORD_COMPONENT.into(),
            entry_len,
            entry_digest,
        );

        let weak_inner = Arc::downgrade(&self.inner);
        let se_dispose = socket_epoch.to_owned();
        let id_dispose = id.clone();
        let held = Arc::new(HeldLease {
            lease: lease.clone(),
            session_id: session_id.to_owned(),
            shared,
            admissions: Mutex::new(HashSet::from([admission_id.to_owned()])),
            reads: CompactReadLease::new(move || {
                if let Some(inner) = weak_inner.upgrade() {
                    Self::dispose_in(&inner, &se_dispose, &id_dispose);
                }
            }),
            released_flag: AtomicBool::new(false),
        });

        let peer = st.peers.entry(socket_epoch.to_owned()).or_default();
        peer.held.insert(id, held);
        st.open += 1;
        Ok(lease)
    }

    pub fn claim(
        &self,
        caller: &RecoveryCaller,
        lease_id: &str,
        admission_id: &str,
    ) -> RecoveryOutcome<Arc<HeldLease>> {
        let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(held) = st
            .peers
            .get(&caller.socket_epoch)
            .and_then(|p| p.held.get(lease_id))
        else {
            return Err(repair_needed(RepairReason::UnknownToken));
        };
        let has_adm = held
            .admissions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(admission_id);
        if held.session_id != caller.session_id || !has_adm {
            return Err(repair_needed(RepairReason::UnknownToken));
        }
        recovered(held.clone())
    }

    pub async fn page(
        &self,
        held: &HeldLease,
        component_ref: &str,
        offset: usize,
        signal: Option<&CancelSignal>,
    ) -> RecoveryOutcome<RecoveryPage> {
        let staging = {
            self.inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .staging
                .clone()
        };
        let Some(staging) = staging else {
            return Err(storage_unavailable(StorageUnavailableReason::Range));
        };
        if component_ref != EVENT_RECORD_COMPONENT {
            return Err(storage_unavailable(StorageUnavailableReason::Range));
        }

        let res = held
            .reads
            .run(async {
                let open_fut = {
                    let mut shared = held.shared.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(existing) = &shared.opened {
                        existing.clone()
                    } else {
                        let content = shared.content.clone();
                        let st = staging.clone();
                        let fut: SharedOpenFut = async move {
                            let ok = || Ok(());
                            st.open(&content, &ok).await
                        }
                        .boxed()
                        .shared();
                        shared.opened = Some(fut.clone());
                        fut
                    }
                };
                let bytes = open_fut.await.map_err(|err| match err {
                    RecoveryBytesError::Integrity => {
                        crate::compact_read_lease::CompactError::Io("integrity".into())
                    }
                    other => crate::compact_read_lease::CompactError::Io(other.to_string()),
                })?;
                if signal.is_some_and(CancelSignal::is_aborted) {
                    return Err(crate::compact_read_lease::CompactError::Io(
                        "aborted".into(),
                    ));
                }
                let valid = || held.reads.assert();
                produce_page(
                    bytes.as_ref(),
                    offset,
                    Some(&|| valid().map_err(Into::into)),
                )
                .await
                .map_err(|err| match err {
                    RecoveryBytesError::Released => crate::compact_read_lease::CompactError::read(
                        crate::compact_read_lease::CompactReadReason::Released,
                    ),
                    RecoveryBytesError::Range => crate::compact_read_lease::CompactError::read(
                        crate::compact_read_lease::CompactReadReason::Range,
                    ),
                    RecoveryBytesError::SizeLimit => crate::compact_read_lease::CompactError::read(
                        crate::compact_read_lease::CompactReadReason::SizeLimit,
                    ),
                    RecoveryBytesError::CountLimit => {
                        crate::compact_read_lease::CompactError::read(
                            crate::compact_read_lease::CompactReadReason::CountLimit,
                        )
                    }
                    RecoveryBytesError::Ownership => crate::compact_read_lease::CompactError::read(
                        crate::compact_read_lease::CompactReadReason::Ownership,
                    ),
                    RecoveryBytesError::Retired => crate::compact_read_lease::CompactError::read(
                        crate::compact_read_lease::CompactReadReason::Retired,
                    ),
                    RecoveryBytesError::Integrity => {
                        crate::compact_read_lease::CompactError::Io("integrity".into())
                    }
                    RecoveryBytesError::Aborted => {
                        crate::compact_read_lease::CompactError::Io("aborted".into())
                    }
                    other => crate::compact_read_lease::CompactError::Io(other.to_string()),
                })
            })
            .await;

        match res {
            Ok(page) => recovered(page),
            Err(err) => {
                held.shared
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .opened = None;
                if err.read_reason() == Some(crate::compact_read_lease::CompactReadReason::Released)
                {
                    return Err(repair_needed(RepairReason::Released));
                }
                let reason = match err.read_reason() {
                    Some(crate::compact_read_lease::CompactReadReason::Range) => {
                        StorageUnavailableReason::Range
                    }
                    Some(crate::compact_read_lease::CompactReadReason::SizeLimit) => {
                        StorageUnavailableReason::SizeLimit
                    }
                    Some(crate::compact_read_lease::CompactReadReason::CountLimit) => {
                        StorageUnavailableReason::CountLimit
                    }
                    Some(crate::compact_read_lease::CompactReadReason::Ownership) => {
                        StorageUnavailableReason::Ownership
                    }
                    Some(crate::compact_read_lease::CompactReadReason::Retired) => {
                        StorageUnavailableReason::Retired
                    }
                    _ => match err.to_string().as_str() {
                        "integrity" => StorageUnavailableReason::Integrity,
                        "aborted" => StorageUnavailableReason::Aborted,
                        _ => StorageUnavailableReason::Unavailable,
                    },
                };
                Err(storage_unavailable(reason))
            }
        }
    }

    pub fn release(
        &self,
        caller: &RecoveryCaller,
        lease_id: &str,
        admission_id: &str,
    ) -> RecoveryOutcome<ReleaseReceipt> {
        let mut to_release: Option<Arc<HeldLease>> = None;
        let receipt = {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(peer) = st.peers.get_mut(&caller.socket_epoch) else {
                return Err(repair_needed(RepairReason::UnknownToken));
            };
            let make_receipt = |outcome: ReleaseOutcome, remaining: usize| {
                recovered(ReleaseReceipt {
                    lease: LeaseRefIdentity {
                        kind: "event".into(),
                        id: lease_id.to_owned(),
                    },
                    outcome,
                    remaining,
                })
            };
            if let Some(held) = peer.held.get(lease_id).cloned()
                && held.session_id == caller.session_id
            {
                let (removed, remaining) = {
                    let mut adms = held
                        .admissions
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    let removed = adms.remove(admission_id);
                    (removed, adms.len())
                };
                if removed {
                    Self::remember(peer, lease_id, &held.session_id, admission_id);
                    if remaining > 0 {
                        return make_receipt(ReleaseOutcome::Held, remaining);
                    }
                    st.released += 1;
                    to_release = Some(held);
                    make_receipt(ReleaseOutcome::Released, 0)
                } else if peer.released.get(lease_id).is_some_and(|r| {
                    r.session_id == caller.session_id && r.admissions.contains(admission_id)
                }) {
                    st.duplicates += 1;
                    make_receipt(ReleaseOutcome::Duplicate, remaining)
                } else {
                    return Err(repair_needed(RepairReason::UnknownToken));
                }
            } else if peer.released.get(lease_id).is_some_and(|r| {
                r.session_id == caller.session_id && r.admissions.contains(admission_id)
            }) {
                st.duplicates += 1;
                make_receipt(ReleaseOutcome::Duplicate, 0)
            } else {
                return Err(repair_needed(RepairReason::UnknownToken));
            }
        };
        if let Some(held) = to_release {
            held.release_once();
        }
        receipt
    }

    pub fn abandon(&self, socket_epoch: &str, lease_id: &str) {
        let held = {
            self.inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .peers
                .get(socket_epoch)
                .and_then(|p| p.held.get(lease_id))
                .cloned()
        };
        if let Some(held) = held
            && held.release_once()
        {
            self.inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .abandoned += 1;
        }
    }

    pub fn forget_admission(&self, caller: &RecoveryCaller, admission_id: &str) {
        let to_abandon: Vec<String> = {
            let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(peer) = st.peers.get(&caller.socket_epoch) else {
                return;
            };
            let mut list = Vec::new();
            for held in peer.held.values() {
                if held.session_id != caller.session_id {
                    continue;
                }
                let mut adms = held
                    .admissions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if adms.remove(admission_id) && adms.is_empty() {
                    list.push(held.lease.id.clone());
                }
            }
            list
        };
        for lease_id in to_abandon {
            self.abandon(&caller.socket_epoch, &lease_id);
        }
    }

    pub fn forget_peer(&self, socket_epoch: &str) {
        let held_list: Vec<Arc<HeldLease>> = {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            st.peers
                .remove(socket_epoch)
                .map(|p| p.held.into_values().collect())
                .unwrap_or_default()
        };
        for held in held_list {
            // Releasing when the peer was already removed triggers dispose_in cleanly.
            if held.release_once() {
                let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                let mut s_guard = held.shared.lock().unwrap_or_else(PoisonError::into_inner);
                s_guard.holders = s_guard.holders.saturating_sub(1);
                if s_guard.holders == 0 {
                    if let Some(key) = s_guard.key {
                        st.shared.remove(&key);
                    }
                    if let Some(staging) = &st.staging {
                        staging.release(&s_guard.content);
                        st.content_bytes = st
                            .content_bytes
                            .saturating_sub(s_guard.content.ref_.entry().length);
                    }
                }
                st.open = st.open.saturating_sub(1);
                let listeners = st.ended.clone();
                drop(s_guard);
                drop(st);
                for l in listeners {
                    l(&held.lease.id);
                }
            }
        }
    }

    pub fn status(&self) -> EventLeaseCounters {
        let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        EventLeaseCounters {
            open: st.open,
            peers: st.peers.len(),
            minted: st.minted,
            released: st.released,
            duplicates: st.duplicates,
            abandoned: st.abandoned,
            content_bytes: st.content_bytes,
            refused: st.refusals.clone(),
        }
    }

    fn stage_in(
        st: &mut EventLeasesState,
        record: &Value,
        content: RecordContent,
        shared_key: Option<usize>,
    ) -> Result<Arc<Mutex<SharedContent>>, ()> {
        if let Some(key) = shared_key
            && let Some(existing) = st.shared.get(&key)
            && existing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .holders
                > 0
        {
            return Ok(existing.clone());
        }
        let staging = st.staging.get_or_insert_with(|| {
            let dir =
                std::env::temp_dir().join(format!("senpi-event-leases-{}", uuid::Uuid::new_v4()));
            TransferStaging::new(
                CompactContentStore::new(CompactContentStoreOptions {
                    dir: dir.display().to_string(),
                    ..Default::default()
                }),
                None,
            )
        });
        let staged = match stage_record(staging, record, content) {
            Ok(s) => s,
            Err(StagingOpError::Transfer(err)) => {
                eprintln!("senpi rpc event lease: record not staged ({err})");
                return Err(());
            }
            Err(StagingOpError::Bytes(err)) => {
                eprintln!("senpi rpc event lease: record not staged ({err})");
                return Err(());
            }
        };
        if let Err(err) = staging.adopt(&staged) {
            staging.release(&staged);
            eprintln!("senpi rpc event lease: record not staged ({err})");
            return Err(());
        }
        st.content_bytes += staged.ref_.entry().length;
        let shared = Arc::new(Mutex::new(SharedContent {
            key: shared_key,
            content: staged,
            holders: 0,
            opened: None,
        }));
        if let Some(key) = shared_key {
            st.shared.insert(key, shared.clone());
        }
        Ok(shared)
    }

    fn remember(peer: &mut PeerLeases, lease_id: &str, session_id: &str, admission_id: &str) {
        let entry = peer.released.entry(lease_id.to_owned()).or_insert_with(|| {
            peer.released_order.push_back(lease_id.to_owned());
            ReleasedRecord {
                session_id: session_id.to_owned(),
                admissions: HashSet::new(),
            }
        });
        entry.admissions.insert(admission_id.to_owned());
        while peer.released.len() > RELEASED_LEASES_REMEMBERED {
            if let Some(oldest) = peer.released_order.pop_front() {
                peer.released.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn dispose_in(inner: &Arc<Mutex<EventLeasesState>>, socket_epoch: &str, lease_id: &str) {
        let (listeners, held) = {
            let mut st = inner.lock().unwrap_or_else(PoisonError::into_inner);
            let held = st
                .peers
                .get_mut(socket_epoch)
                .and_then(|peer| peer.held.remove(lease_id));
            let Some(held) = held else {
                return;
            };
            st.open = st.open.saturating_sub(1);
            let mut s_guard = held.shared.lock().unwrap_or_else(PoisonError::into_inner);
            s_guard.holders = s_guard.holders.saturating_sub(1);
            if s_guard.holders == 0 {
                if let Some(key) = s_guard.key {
                    st.shared.remove(&key);
                }
                if let Some(staging) = &st.staging {
                    staging.release(&s_guard.content);
                    st.content_bytes = st
                        .content_bytes
                        .saturating_sub(s_guard.content.ref_.entry().length);
                }
            }
            drop(s_guard);
            (st.ended.clone(), held)
        };
        for l in listeners {
            l(&held.lease.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn socket_page_limit_counts_independent_pending_leases() {
        use crate::compact_recovery_requests::{
            RecoveryPageFailure, RecoveryPageRequest, RecoveryPageRequests, RequestRefusalReason,
            SOCKET_PAGE_REQUESTS_MAX,
        };
        use crate::compact_recovery_state::RecoveryCheckpoints;

        let leases = EventLeases::default();
        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "s1".into(),
        };
        let mut pages = Vec::new();
        for index in 0..=SOCKET_PAGE_REQUESTS_MAX {
            let record = json!({"type": "message_end", "index": index});
            let admission = format!("adm-{index}");
            let lease = leases
                .mint(
                    "se-1",
                    "s1",
                    &admission,
                    "ep-1",
                    1,
                    &record,
                    RecordContent::Text(record.to_string()),
                    None,
                )
                .expect("mint");
            let held = leases.claim(&caller, &lease.id, &admission).expect("claim");
            held.shared.lock().expect("shared").opened = Some(
                std::future::pending::<Result<Arc<dyn FrozenBytes>, RecoveryBytesError>>()
                    .boxed()
                    .shared(),
            );
            pages.push(RecoveryPageRequest {
                request_id: format!("req-{index}"),
                admission_id: admission,
                token: None,
                lease: Some(("event".into(), lease.id)),
                component_ref: EVENT_RECORD_COMPONENT.into(),
                cursor: None,
            });
        }
        let requests = Arc::new(RecoveryPageRequests::new(
            RecoveryCheckpoints::default(),
            None,
            Some(leases),
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(pages.len() + 1));
        let mut tasks = tokio::task::JoinSet::new();
        for page in pages {
            let requests = Arc::clone(&requests);
            let barrier = Arc::clone(&barrier);
            let caller = caller.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                requests.request(&caller, &page, None).await
            });
        }
        barrier.wait().await;
        let refused = tokio::time::timeout(std::time::Duration::from_secs(5), tasks.join_next())
            .await
            .expect("request deadline")
            .expect("task")
            .expect("join")
            .expect_err("socket limit");
        assert!(
            matches!(refused, RecoveryPageFailure::RequestRefused(refusal)
            if refusal.reason == RequestRefusalReason::RequestLimit)
        );
        assert_eq!(requests.status().outstanding, SOCKET_PAGE_REQUESTS_MAX);
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        assert_eq!(requests.status().outstanding, 0);
    }

    #[tokio::test]
    async fn cancelling_page_request_releases_socket_budget_and_token() {
        use crate::compact_recovery_requests::{RecoveryPageRequest, RecoveryPageRequests};
        use crate::compact_recovery_state::RecoveryCheckpoints;
        use std::task::Poll;

        let leases = EventLeases::default();
        let record = json!({"type": "message_end", "payload": "hello"});
        let lease = leases
            .mint(
                "se-1",
                "s1",
                "adm-1",
                "ep-1",
                1,
                &record,
                RecordContent::Text(record.to_string()),
                None,
            )
            .expect("mint");
        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "s1".into(),
        };
        let held = leases.claim(&caller, &lease.id, "adm-1").expect("claim");
        held.shared.lock().expect("shared").opened = Some(
            std::future::pending::<Result<Arc<dyn FrozenBytes>, RecoveryBytesError>>()
                .boxed()
                .shared(),
        );
        let requests =
            RecoveryPageRequests::new(RecoveryCheckpoints::default(), None, Some(leases));
        let request = RecoveryPageRequest {
            request_id: "req-1".into(),
            admission_id: "adm-1".into(),
            token: None,
            lease: Some(("event".into(), lease.id)),
            component_ref: EVENT_RECORD_COMPONENT.into(),
            cursor: None,
        };
        let mut page = Box::pin(requests.request(&caller, &request, None));
        std::future::poll_fn(|cx| {
            assert!(page.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(requests.status().outstanding, 1);
        drop(page);
        assert_eq!(requests.status().outstanding, 0);
        held.shared.lock().expect("shared").opened = None;
        assert!(requests.request(&caller, &request, None).await.is_ok());
    }

    #[tokio::test]
    async fn mints_pages_counts_multi_admission_receipts_and_detects_duplicates() {
        let leases = EventLeases::default();
        let record = json!({"type":"message_end","payload":"hello"});
        let text = serde_json::to_string(&record).expect("json");
        let lease = leases
            .mint(
                "se-1",
                "s1",
                "adm-1",
                "ep-1",
                10,
                &record,
                RecordContent::Text(format!("{text}\n")),
                None,
            )
            .expect("mint");
        let caller = RecoveryCaller {
            socket_epoch: "se-1".into(),
            session_id: "s1".into(),
        };
        let held = leases.claim(&caller, &lease.id, "adm-1").expect("claim");
        held.add_admission("adm-2");

        let page = leases
            .page(&held, EVENT_RECORD_COMPONENT, 0, None)
            .await
            .expect("page");
        assert!(page.complete);

        let r1 = leases.release(&caller, &lease.id, "adm-1").expect("r1");
        assert_eq!(r1.outcome, ReleaseOutcome::Held);
        assert_eq!(r1.remaining, 1);

        let dup = leases.release(&caller, &lease.id, "adm-1").expect("dup");
        assert_eq!(dup.outcome, ReleaseOutcome::Duplicate);

        let r2 = leases.release(&caller, &lease.id, "adm-2").expect("r2");
        assert_eq!(r2.outcome, ReleaseOutcome::Released);
        assert_eq!(r2.remaining, 0);
        assert_eq!(leases.status().open, 0);
        assert_eq!(leases.status().content_bytes, 0);
    }

    #[test]
    fn enforces_peer_lease_limit_and_abandon_cleanup() {
        let leases = EventLeases::new(2);
        let record = json!({"a":1});
        let l1 = leases
            .mint(
                "se-1",
                "s1",
                "a1",
                "ep-1",
                1,
                &record,
                RecordContent::Text("{\"a\":1}".into()),
                None,
            )
            .expect("l1");
        let _l2 = leases
            .mint(
                "se-1",
                "s1",
                "a2",
                "ep-1",
                2,
                &record,
                RecordContent::Text("{\"a\":1}".into()),
                None,
            )
            .expect("l2");
        let err = leases
            .mint(
                "se-1",
                "s1",
                "a3",
                "ep-1",
                3,
                &record,
                RecordContent::Text("{\"a\":1}".into()),
                None,
            )
            .expect_err("limit");
        assert_eq!(err, EventLeaseRefusal::LeaseLimit);

        leases.abandon("se-1", &l1.id);
        assert_eq!(leases.status().abandoned, 1);
        assert_eq!(leases.status().open, 1);
        leases.forget_peer("se-1");
        assert_eq!(leases.status().open, 0);
    }
}
