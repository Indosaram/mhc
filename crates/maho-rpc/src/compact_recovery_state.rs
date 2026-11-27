//! senpi `modes/rpc/compact-recovery-state.ts`: the checkpoint tokens of one host (S08a/S08b,
//! R3.C1.2-C1.5). Freezes complete accepted roots, mints per-stream generations, invalidates
//! superseded tokens immediately while letting in-flight reads unwind, and counts refusals.

use crate::accepted_transcript::TranscriptRoot;
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::{CompactError, CompactReadLease, CompactReadReason};
use crate::compact_recovery_bytes::{CancelSignal, RecoveryBytesError, RecoveryPage};
use crate::compact_recovery_catalog::CatalogRead;
pub use crate::compact_recovery_checkpoint_types::{CheckpointOps, RetainedRoot};
use crate::compact_recovery_manifest::RecoveryManifests;
use crate::compact_recovery_manifest_shape::RecoveryManifest;
use crate::compact_recovery_staleness::{
    LaneStalenessView, retired_token_failure, token_staleness,
};
pub use crate::compact_recovery_streams::RecoveryRoute;
use crate::compact_recovery_streams::RecoveryStreams;
use crate::compact_recovery_types::{
    CheckpointToken, RecoveryAdmission, RecoveryCaller, RecoveryCounters, RecoveryCutIdentity,
    RecoveryFailure, RecoveryOutcome, RecoveryStreamState, RecoveryToken, RefusalReason,
    RepairReason, StorageUnavailableReason, recovered, refusal_reason, repair_needed,
    storage_unavailable,
};
use crate::compact_settled_messages::SettledRead;
use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

pub(crate) struct FreezeCommitVersion(Mutex<Arc<()>>);

impl FreezeCommitVersion {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(Arc::new(())))
    }

    pub(crate) fn advance(&self) -> Arc<()> {
        let mut current = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *current = Arc::new(());
        Arc::clone(&current)
    }

    pub(crate) fn matches(&self, expected: &Arc<()>) -> bool {
        Arc::ptr_eq(
            &self.0.lock().unwrap_or_else(PoisonError::into_inner),
            expected,
        )
    }
}

/// RULING-119 route retirement proof (from `route-retire-reasons.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteRetirement {
    ClientDetach,
    ReopenSuperseded,
    OpenEndedAnnounced,
}

pub struct FrozenLaneCut {
    pub root_key: usize,
    pub root: FrozenAcceptedRoot,
    pub store: Option<CompactContentStore>,
    pub has_current: bool,
}

/// Host lane operations needed by `RecoveryCheckpoints` (implemented by `SessionAcceptanceLane`
/// wrappers or test doubles).
pub trait RecoveryLane: Send + Sync {
    fn session_epoch(&self) -> String;
    fn accepted_sequence(&self) -> u64;
    fn retired(&self) -> bool;
    fn retire_reason(&self) -> Option<RetireReason>;
    fn next_generation(&self) -> Option<u64>;
    fn freeze_cut(&self) -> Option<FrozenLaneCut>;
    fn release_cut(&self, root_key: usize);
    fn read_settled(&self, root_key: usize) -> BoxFuture<'static, Result<SettledRead, String>>;
    fn read_current(
        &self,
        root_key: usize,
    ) -> BoxFuture<'static, Result<Value, RecoveryBytesError>>;
    fn read_components(
        &self,
        root_key: usize,
    ) -> BoxFuture<'static, Result<Value, RecoveryBytesError>>;
    fn read_active(&self, root_key: usize)
    -> BoxFuture<'static, Result<Value, RecoveryBytesError>>;
    fn read_calls(&self, root_key: usize) -> BoxFuture<'static, Result<Value, RecoveryBytesError>>;
}

/// Lease handle combining `CompactReadLease` validity/operation unwinding with `retain()` and `hold()`.
struct CheckpointLease {
    inner: Arc<CompactReadLease>,
    state: Arc<Mutex<CheckpointLeaseState>>,
}

struct CheckpointLeaseState {
    released: bool,
    retains: usize,
    operations: usize,
    dispose: Option<Box<dyn FnOnce() + Send>>,
}

struct CheckpointOperation(Arc<Mutex<CheckpointLeaseState>>);

impl Drop for CheckpointOperation {
    fn drop(&mut self) {
        let dispose = {
            let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.operations -= 1;
            if state.released && state.operations == 0 && state.retains == 0 {
                state.dispose.take()
            } else {
                None
            }
        };
        if let Some(dispose) = dispose {
            dispose();
        }
    }
}

impl CheckpointLease {
    fn new(
        dispose: impl FnOnce() + Send + 'static,
        valid: impl Fn() -> Result<(), CompactError> + Send + Sync + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(CheckpointLeaseState {
            released: false,
            retains: 0,
            operations: 0,
            dispose: Some(Box::new(dispose)),
        }));
        let inner = Arc::new(CompactReadLease::with_valid(|| {}, valid));
        Self { inner, state }
    }

    fn assert(&self) -> Result<(), CompactError> {
        if self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .released
        {
            return Err(CompactError::read(CompactReadReason::Released));
        }
        self.inner.assert()
    }

    async fn run<T, E>(
        &self,
        read: impl Future<Output = Result<T, E>>,
        map_compact: fn(CompactError) -> E,
    ) -> Result<T, E> {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.released {
                return Err(map_compact(CompactError::read(CompactReadReason::Released)));
            }
            state.operations += 1;
        }
        let _guard = CheckpointOperation(self.state.clone());
        if let Err(err) = self.assert() {
            return Err(map_compact(err));
        }
        if self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .released
        {
            return Err(map_compact(CompactError::read(CompactReadReason::Released)));
        }
        let value = read.await?;
        if let Err(err) = self.assert() {
            return Err(map_compact(err));
        }
        Ok(value)
    }

    async fn hold<T>(&self, read: impl Future<Output = T>) -> T {
        {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .operations += 1;
        }
        let _guard = CheckpointOperation(self.state.clone());
        read.await
    }

    fn retain(&self, root_key: usize) -> RetainedRoot {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retains += 1;
        let state = self.state.clone();
        let ended = Arc::new(AtomicBool::new(false));
        RetainedRoot::new(root_key, move || {
            if ended.swap(true, Ordering::SeqCst) {
                return;
            }
            let dispose = {
                let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
                st.retains -= 1;
                if st.released && st.operations == 0 && st.retains == 0 {
                    st.dispose.take()
                } else {
                    None
                }
            };
            if let Some(d) = dispose {
                d();
            }
        })
    }

    fn release(&self) -> bool {
        let (first, dispose) = {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.released {
                return false;
            }
            st.released = true;
            let dispose = if st.operations == 0 && st.retains == 0 {
                st.dispose.take()
            } else {
                None
            };
            (true, dispose)
        };
        self.inner.release();
        if let Some(d) = dispose {
            d();
        }
        first
    }
}

#[derive(Clone)]
pub struct FrozenCheckpoint {
    pub token: CheckpointToken,
    pub root_key: usize,
    pub root: FrozenAcceptedRoot,
    pub lane: Arc<dyn RecoveryLane>,
    store: Option<CompactContentStore>,
    lease: Arc<CheckpointLease>,
}

impl FrozenCheckpoint {
    pub async fn hold<T>(&self, read: impl Future<Output = T>) -> T {
        self.lease.hold(read).await
    }

    pub fn retain(&self) -> RetainedRoot {
        self.lease.retain(self.root_key)
    }
}

pub type TokenEndListener = Arc<dyn Fn(&str) + Send + Sync>;

const RETIRED_TOKEN_HISTORY_LIMIT: usize = 4096;

#[derive(Default)]
struct IssuedTokens {
    entries: HashMap<String, RecoveryAdmission>,
    retired: VecDeque<String>,
}

impl IssuedTokens {
    fn retire(&mut self, id: &str) {
        if !self.entries.contains_key(id) {
            return;
        }
        self.retired.push_back(id.to_owned());
        while self.retired.len() > RETIRED_TOKEN_HISTORY_LIMIT {
            if let Some(oldest) = self.retired.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

struct CheckpointsInner {
    checkpoints: Mutex<HashMap<String, FrozenCheckpoint>>,
    issued: Mutex<IssuedTokens>,
    streams: Mutex<RecoveryStreams>,
    manifests: Arc<RecoveryManifests>,
    refusals: Mutex<HashMap<RefusalReason, u64>>,
    unavailable: Mutex<HashMap<StorageUnavailableReason, u64>>,
    ended: Mutex<Vec<TokenEndListener>>,
    minted: Mutex<u64>,
    frozen: Mutex<u64>,
    reclaimed: Mutex<u64>,
}

/// Checkpoint tokens of one host (S08a/S08b, R3.C1.2-C1.5).
#[derive(Clone)]
pub struct RecoveryCheckpoints {
    inner: Arc<CheckpointsInner>,
}

impl Default for RecoveryCheckpoints {
    fn default() -> Self {
        Self::new(Arc::new(RecoveryManifests::default()))
    }
}

impl RecoveryCheckpoints {
    pub fn new(manifests: Arc<RecoveryManifests>) -> Self {
        Self {
            inner: Arc::new(CheckpointsInner {
                checkpoints: Mutex::new(HashMap::new()),
                issued: Mutex::new(IssuedTokens::default()),
                streams: Mutex::new(RecoveryStreams::new()),
                manifests,
                refusals: Mutex::new(HashMap::new()),
                unavailable: Mutex::new(HashMap::new()),
                ended: Mutex::new(Vec::new()),
                minted: Mutex::new(0),
                frozen: Mutex::new(0),
                reclaimed: Mutex::new(0),
            }),
        }
    }

    pub fn open(&self) -> usize {
        self.inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn tokens(&self) -> Vec<CheckpointToken> {
        self.inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|c| c.token.clone())
            .collect()
    }

    pub fn stream(&self, caller: &RecoveryCaller) -> Option<RecoveryStreamState> {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .describe(caller)
    }

    pub fn routes_admission(&self, socket_epoch: &str, admission_id: &str) -> bool {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .routes(socket_epoch, admission_id)
    }

    pub fn on_token_end(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        self.inner
            .ended
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::new(listener));
    }

    pub fn status(&self) -> RecoveryCounters {
        RecoveryCounters {
            open: self.open(),
            streams: self
                .inner
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .size(),
            frozen: *self
                .inner
                .frozen
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            reclaimed: *self
                .inner
                .reclaimed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            refused: self
                .inner
                .refusals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
            unavailable: self
                .inner
                .unavailable
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect(),
        }
    }

    /// The next generation of the route's stream: a failed freeze advances nothing and pins nothing.
    pub fn freeze(
        &self,
        route: &RecoveryRoute,
        lane: Arc<dyn RecoveryLane>,
        join: bool,
    ) -> RecoveryOutcome<FrozenCheckpoint> {
        self.freeze_inner(route, lane, join, None)
            .map(|checkpoint| checkpoint.expect("unconditional freeze commits"))
    }

    pub(crate) fn freeze_if_current(
        &self,
        route: &RecoveryRoute,
        lane: Arc<dyn RecoveryLane>,
        version: &FreezeCommitVersion,
        expected: &Arc<()>,
    ) -> RecoveryOutcome<Option<FrozenCheckpoint>> {
        self.freeze_inner(route, lane, false, Some((version, expected)))
    }

    fn freeze_inner(
        &self,
        route: &RecoveryRoute,
        lane: Arc<dyn RecoveryLane>,
        join: bool,
        version: Option<(&FreezeCommitVersion, &Arc<()>)>,
    ) -> RecoveryOutcome<Option<FrozenCheckpoint>> {
        if lane.retired() {
            return self.refuse(repair_needed(RepairReason::LaneRetired));
        }
        if join {
            let current_id = self
                .inner
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&route.caller())
                .and_then(|s| s.current);
            if let Some(current_id) = current_id {
                let current = self
                    .inner
                    .checkpoints
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&current_id)
                    .cloned();
                if let Some(current) = current
                    && self.check(&current.token, &route.caller()).is_ok()
                {
                    self.inner
                        .streams
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .join(route);
                    return recovered(Some(current));
                }
            }
        }
        let Some(cut) = lane.freeze_cut() else {
            return self.refuse(storage_unavailable(
                StorageUnavailableReason::CompactStateUnavailable,
            ));
        };
        if !cut.has_current {
            lane.release_cut(cut.root_key);
            return self.refuse(storage_unavailable(
                StorageUnavailableReason::CompactStateUnavailable,
            ));
        }
        let Some(generation) = lane.next_generation() else {
            lane.release_cut(cut.root_key);
            return self.refuse(storage_unavailable(StorageUnavailableReason::Unavailable));
        };
        let minted = {
            let mut m = self
                .inner
                .minted
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *m += 1;
            *m
        };
        let id = format!("{}:{minted}", route.socket_epoch);
        if self.inner.manifests.pin(&id, cut.root_key).is_err() {
            lane.release_cut(cut.root_key);
            return self.refuse(storage_unavailable(StorageUnavailableReason::Ownership));
        }
        let retired = if let Some((version, expected)) = version {
            let current = version.0.lock().unwrap_or_else(PoisonError::into_inner);
            if Arc::ptr_eq(&current, expected) {
                Some(
                    self.inner
                        .streams
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .next(route, &cut.root.session_epoch, &id, generation),
                )
            } else {
                None
            }
        } else {
            Some(
                self.inner
                    .streams
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .next(route, &cut.root.session_epoch, &id, generation),
            )
        };
        let Some(retired) = retired else {
            self.inner.manifests.release(&id);
            lane.release_cut(cut.root_key);
            return recovered(None);
        };
        let admission = RecoveryAdmission {
            socket_epoch: route.socket_epoch.clone(),
            admission_id: route.admission_id.clone(),
            route_kind: route.route_kind,
            peer: route.peer.clone(),
            session_id: route.session_id.clone(),
            session_epoch: cut.root.session_epoch.clone(),
            generation,
        };
        let token = CheckpointToken::new(
            id.clone(),
            admission.clone(),
            RecoveryCutIdentity {
                session_epoch: cut.root.session_epoch.clone(),
                sequence: cut.root.sequence,
                history_version: cut.root.transcript.history_version,
                leaf_id: cut.root.transcript.leaf_id.clone(),
                active_message_id: cut.root.active_message_id.clone(),
                turn_id: cut.root.turn_id.clone(),
            },
        );

        let inner_dispose = Arc::downgrade(&self.inner);
        let lane_dispose = lane.clone();
        let tok_id_dispose = id.clone();
        let root_key_dispose = cut.root_key;

        let inner_valid = Arc::downgrade(&self.inner);
        let lane_valid = lane.clone();
        let tok_valid = token.clone();

        let lease = Arc::new(CheckpointLease::new(
            move || {
                if let Some(inner) = inner_dispose.upgrade() {
                    inner.manifests.release(&tok_id_dispose);
                    lane_dispose.release_cut(root_key_dispose);
                    inner
                        .checkpoints
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&tok_id_dispose);
                    inner
                        .issued
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .retire(&tok_id_dispose);
                    let listeners = inner
                        .ended
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    for listener in listeners {
                        listener(&tok_id_dispose);
                    }
                } else {
                    lane_dispose.release_cut(root_key_dispose);
                }
            },
            move || {
                let Some(inner) = inner_valid.upgrade() else {
                    return Err(CompactError::read(CompactReadReason::Released));
                };
                if Self::staleness_in(&inner, &tok_valid, lane_valid.as_ref()).is_some() {
                    return Err(CompactError::read(CompactReadReason::Retired));
                }
                Ok(())
            },
        ));

        let checkpoint = FrozenCheckpoint {
            token: token.clone(),
            root_key: cut.root_key,
            root: cut.root,
            lane,
            store: cut.store,
            lease,
        };
        self.inner
            .issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .insert(token.id.clone(), admission);
        self.inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, checkpoint.clone());
        *self
            .inner
            .frozen
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        if let Some(retired_id) = retired {
            self.reclaim(&retired_id);
        }
        recovered(Some(checkpoint))
    }

    pub fn release(&self, token: &RecoveryToken) {
        let cp = self
            .inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.id)
            .cloned();
        if let Some(cp) = cp {
            cp.lease.release();
        }
    }

    pub fn forget_peer(&self, socket_epoch: &str) {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .forget_peer(socket_epoch);
        let ids: Vec<String> = self
            .inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|cp| cp.token.admission.socket_epoch == socket_epoch)
            .map(|cp| cp.token.id.clone())
            .collect();
        for id in ids {
            self.reclaim(&id);
        }
    }

    pub fn forget_session(&self, session_id: &str) {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .forget_session(session_id);
    }

    pub fn retire_route(
        &self,
        caller: &RecoveryCaller,
        admission_id: &str,
        _retirement: RouteRetirement,
    ) {
        let (current, remaining_routes) = {
            let mut streams = self
                .inner
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let current = streams.get(caller).and_then(|s| s.current);
            streams.retire_route(caller, admission_id);
            let remaining = streams.get(caller).map_or(0, |s| s.routes.len());
            (current, remaining)
        };
        if remaining_routes > 0 {
            return;
        }
        if let Some(current_id) = current {
            self.reclaim(&current_id);
        }
        let matching: Vec<String> = self
            .inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|cp| {
                cp.token.admission.socket_epoch == caller.socket_epoch
                    && cp.token.admission.session_id == caller.session_id
                    && cp.token.admission.admission_id == admission_id
            })
            .map(|cp| cp.token.id.clone())
            .collect();
        for id in matching {
            self.reclaim(&id);
        }
    }

    pub fn check(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<RecoveryToken> {
        let _ = self.claim(token, caller)?;
        recovered(token.clone())
    }

    pub fn transcript(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<TranscriptRoot> {
        let cp = self.claim(token, caller)?;
        let transcript = cp.root.transcript;
        match transcript.unavailable {
            None => recovered(transcript),
            Some(reason) => self.refuse(storage_unavailable(reason.into())),
        }
    }

    pub async fn read_settled(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<SettledRead> {
        self.read_with(token, caller, |cp| {
            let lane = cp.lane.clone();
            let key = cp.root_key;
            async move {
                lane.read_settled(key)
                    .await
                    .map_err(|_| RecoveryBytesError::Unavailable)
            }
        })
        .await
    }

    pub async fn read_current(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<Value> {
        self.read_with(token, caller, |cp| {
            let lane = cp.lane.clone();
            let key = cp.root_key;
            async move { lane.read_current(key).await }
        })
        .await
    }

    pub async fn read_components(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<Value> {
        self.read_with(token, caller, |cp| {
            let lane = cp.lane.clone();
            let key = cp.root_key;
            async move { lane.read_components(key).await }
        })
        .await
    }

    pub async fn read_active(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<Value> {
        self.read_with(token, caller, |cp| {
            let lane = cp.lane.clone();
            let key = cp.root_key;
            async move { lane.read_active(key).await }
        })
        .await
    }

    pub async fn read_calls(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<Value> {
        self.read_with(token, caller, |cp| {
            let lane = cp.lane.clone();
            let key = cp.root_key;
            async move { lane.read_calls(key).await }
        })
        .await
    }

    pub async fn catalog(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<RecoveryManifest> {
        let manifests = self.inner.manifests.clone();
        self.read_with(token, caller, |cp| {
            let token = cp.token.clone();
            let store = cp.store.clone();
            let lease = cp.lease.clone();
            async move {
                let store = store.ok_or(RecoveryBytesError::Unavailable)?;
                let valid = || lease.assert().map_err(RecoveryBytesError::from);
                manifests.manifest(&token, &store, &valid).await
            }
        })
        .await
    }

    pub async fn read_catalog(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
        ref_str: &str,
    ) -> RecoveryOutcome<CatalogRead> {
        let manifests = self.inner.manifests.clone();
        let ref_owned = ref_str.to_owned();
        self.read_with(token, caller, |cp| {
            let tok_id = cp.token.id.clone();
            let lease = cp.lease.clone();
            async move {
                let valid = || lease.assert().map_err(RecoveryBytesError::from);
                manifests.read(&tok_id, &ref_owned, &valid).await
            }
        })
        .await
    }

    pub async fn read_catalog_page(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
        ref_str: &str,
        offset: usize,
        signal: Option<&CancelSignal>,
    ) -> RecoveryOutcome<RecoveryPage> {
        let manifests = self.inner.manifests.clone();
        let ref_owned = ref_str.to_owned();
        let sig_cloned = signal.cloned();
        self.read_with(token, caller, |cp| {
            let token = cp.token.clone();
            let store = cp.store.clone();
            let root_key = cp.root_key;
            let transcript = cp.root.transcript.clone();
            let lease = cp.lease.clone();
            async move {
                let store = store.ok_or(RecoveryBytesError::Unavailable)?;
                let valid = || lease.assert().map_err(RecoveryBytesError::from);
                manifests
                    .page(
                        &token,
                        &store,
                        root_key,
                        &transcript,
                        &ref_owned,
                        offset,
                        &valid,
                        sig_cloned.as_ref(),
                    )
                    .await
            }
        })
        .await
    }

    pub fn verified_components(&self) -> usize {
        self.inner.manifests.verified_components()
    }

    async fn read_with<T, F, Fut>(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
        op: F,
    ) -> RecoveryOutcome<T>
    where
        F: FnOnce(&FrozenCheckpoint) -> Fut,
        Fut: Future<Output = Result<T, RecoveryBytesError>>,
    {
        let cp = self.claim(token, caller)?;
        let res = cp.lease.run(op(&cp), RecoveryBytesError::from).await;
        match res {
            Ok(value) => recovered(value),
            Err(err) => self.refused(token, cp.lane.as_ref(), err),
        }
    }

    fn reclaim(&self, id: &str) {
        let cp = self
            .inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned();
        if let Some(cp) = cp
            && cp.lease.release()
        {
            *self
                .inner
                .reclaimed
                .lock()
                .unwrap_or_else(PoisonError::into_inner) += 1;
        }
    }

    fn claim(
        &self,
        token: &RecoveryToken,
        caller: &RecoveryCaller,
    ) -> RecoveryOutcome<FrozenCheckpoint> {
        let admission = self
            .inner
            .issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get(&token.id)
            .cloned();
        let Some(admission) = admission else {
            return self.refuse(repair_needed(RepairReason::UnknownToken));
        };
        if admission.socket_epoch != caller.socket_epoch
            || admission.session_id != caller.session_id
        {
            return self.refuse(repair_needed(RepairReason::UnknownToken));
        }
        let cp = self
            .inner
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.id)
            .cloned();
        let Some(cp) = cp else {
            let stream = self
                .inner
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&admission.caller());
            return self.refuse(retired_token_failure(stream.as_ref(), &admission));
        };
        if let Err(err) = cp.lease.assert() {
            return self.refused(token, cp.lane.as_ref(), RecoveryBytesError::from(err));
        }
        recovered(cp)
    }

    fn staleness_in(
        inner: &CheckpointsInner,
        token: &CheckpointToken,
        lane: &dyn RecoveryLane,
    ) -> Option<RecoveryFailure> {
        let stream = inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.admission.caller());
        let view = LaneStalenessView {
            retired: lane.retired(),
            retire_reason: lane.retire_reason(),
            session_epoch: lane.session_epoch(),
        };
        token_staleness(stream.as_ref(), token, &view)
    }

    fn refused<T>(
        &self,
        token: &CheckpointToken,
        lane: &dyn RecoveryLane,
        cause: RecoveryBytesError,
    ) -> RecoveryOutcome<T> {
        if let Some(stale) = Self::staleness_in(&self.inner, token, lane) {
            return self.refuse(stale);
        }
        if cause == RecoveryBytesError::Released {
            return self.refuse(repair_needed(RepairReason::Released));
        }
        self.refuse(storage_unavailable(StorageUnavailableReason::from(&cause)))
    }

    fn refuse<T>(&self, failure: RecoveryFailure) -> RecoveryOutcome<T> {
        let reason = refusal_reason(&failure);
        *self
            .inner
            .refusals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(reason)
            .or_insert(0) += 1;
        if let RecoveryFailure::StorageUnavailable { reason: detail } = failure {
            *self
                .inner
                .unavailable
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(detail)
                .or_insert(0) += 1;
        }
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_recovery_types::RecoveryRouteKind;
    use std::sync::atomic::AtomicU64;

    #[tokio::test]
    async fn cancelled_hold_releases_retired_checkpoint() {
        let disposed = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&disposed);
        let lease = CheckpointLease::new(
            move || {
                observed.store(true, Ordering::SeqCst);
            },
            || Ok(()),
        );
        let mut held = Box::pin(lease.hold(std::future::pending::<()>()));
        std::future::poll_fn(|cx| {
            assert!(held.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(lease.release());
        assert!(!disposed.load(Ordering::SeqCst));
        drop(held);
        assert!(
            disposed.load(Ordering::SeqCst),
            "cancelled hold must release its operation"
        );
    }

    #[tokio::test]
    async fn release_during_validation_does_not_start_read_after_disposal() {
        let disposed = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&disposed);
        let slot = Arc::new(Mutex::new(std::sync::Weak::<CheckpointLease>::new()));
        let callback_slot = Arc::clone(&slot);
        let lease = Arc::new(CheckpointLease::new(
            move || {
                observed.store(true, Ordering::SeqCst);
            },
            move || {
                callback_slot
                    .lock()
                    .expect("slot")
                    .upgrade()
                    .expect("lease")
                    .release();
                Ok(())
            },
        ));
        *slot.lock().expect("slot") = Arc::downgrade(&lease);
        let did_read = AtomicBool::new(false);
        let result = lease
            .run(
                async {
                    did_read.store(true, Ordering::SeqCst);
                    Ok::<(), CompactError>(())
                },
                |error| error,
            )
            .await;
        assert!(result.is_err());
        assert!(
            !did_read.load(Ordering::SeqCst),
            "released checkpoint must not start reading"
        );
        assert!(disposed.load(Ordering::SeqCst));
    }

    struct RealLaneWrap {
        lane: Mutex<crate::session_acceptance_lane::SessionAcceptanceLane>,
        store: CompactContentStore,
        gen_counter: AtomicU64,
        next_key: AtomicU64,
        retired_flag: AtomicBool,
        retire_reason: Mutex<Option<RetireReason>>,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
        released_keys: Mutex<Vec<usize>>,
    }

    impl RealLaneWrap {
        fn new(dir: &tempfile::TempDir) -> Self {
            let mut lane = crate::session_acceptance_lane::SessionAcceptanceLane::default();
            lane.bootstrap(Default::default(), Some("nonce-1".into()));
            Self {
                lane: Mutex::new(lane),
                store: CompactContentStore::new(CompactContentStoreOptions {
                    dir: dir.path().join("store").display().to_string(),
                    ..Default::default()
                }),
                gen_counter: AtomicU64::new(0),
                next_key: AtomicU64::new(1),
                retired_flag: AtomicBool::new(false),
                retire_reason: Mutex::new(None),
                roots: Mutex::new(HashMap::new()),
                released_keys: Mutex::new(Vec::new()),
            }
        }
    }

    impl RecoveryLane for RealLaneWrap {
        fn session_epoch(&self) -> String {
            self.lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .session_epoch()
                .to_owned()
        }
        fn accepted_sequence(&self) -> u64 {
            3
        }
        fn retired(&self) -> bool {
            self.retired_flag.load(Ordering::SeqCst)
        }
        fn retire_reason(&self) -> Option<RetireReason> {
            *self
                .retire_reason
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        }
        fn next_generation(&self) -> Option<u64> {
            Some(self.gen_counter.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn freeze_cut(&self) -> Option<FrozenLaneCut> {
            let root = self
                .lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .freeze();
            let key = self.next_key.fetch_add(1, Ordering::SeqCst) as usize;
            self.roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, root.clone());
            Some(FrozenLaneCut {
                root_key: key,
                root,
                store: Some(self.store.clone()),
                has_current: true,
            })
        }
        fn release_cut(&self, root_key: usize) {
            if let Some(root) = self
                .roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&root_key)
            {
                self.lane
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&root);
            }
            self.released_keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(root_key);
        }
        fn read_settled(&self, _: usize) -> BoxFuture<'static, Result<SettledRead, String>> {
            Box::pin(async {
                Ok(SettledRead {
                    settled_messages: Vec::new(),
                    owners: Vec::new(),
                })
            })
        }
        fn read_current(&self, _: usize) -> BoxFuture<'static, Result<Value, RecoveryBytesError>> {
            Box::pin(async { Ok(serde_json::json!({"ok":true})) })
        }
        fn read_components(
            &self,
            _: usize,
        ) -> BoxFuture<'static, Result<Value, RecoveryBytesError>> {
            Box::pin(async { Ok(serde_json::json!({})) })
        }
        fn read_active(&self, _: usize) -> BoxFuture<'static, Result<Value, RecoveryBytesError>> {
            Box::pin(async { Ok(serde_json::json!([])) })
        }
        fn read_calls(&self, _: usize) -> BoxFuture<'static, Result<Value, RecoveryBytesError>> {
            Box::pin(async { Ok(serde_json::json!({"calls":[]})) })
        }
    }

    fn route(se: &str, adm: &str) -> RecoveryRoute {
        RecoveryRoute {
            socket_epoch: se.into(),
            admission_id: adm.into(),
            route_kind: RecoveryRouteKind::Ordinary,
            peer: "peer-1".into(),
            session_id: "s1".into(),
        }
    }

    #[tokio::test]
    async fn retired_checkpoint_history_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lane = Arc::new(RealLaneWrap::new(&dir));
        let checkpoints = RecoveryCheckpoints::default();
        let route = route("se-1", "adm-1");
        let oldest = checkpoints
            .freeze(&route, lane.clone(), false)
            .expect("first");
        for _ in 1..4099 {
            checkpoints
                .freeze(&route, lane.clone(), false)
                .expect("freeze");
        }
        let recent = checkpoints
            .freeze(&route, lane.clone(), false)
            .expect("recent");
        checkpoints
            .freeze(&route, lane.clone(), false)
            .expect("latest");
        assert!(
            checkpoints
                .inner
                .issued
                .lock()
                .expect("issued")
                .entries
                .len()
                <= 4097,
            "retired token history must be bounded in addition to the live token"
        );
        assert_eq!(
            checkpoints.check(&oldest.token, &route.caller()),
            Err(repair_needed(RepairReason::UnknownToken))
        );
        assert_eq!(
            checkpoints.check(&recent.token, &route.caller()),
            Err(crate::compact_recovery_types::superseded_token(4101))
        );
        checkpoints.forget_peer("se-1");
        lane.store.idle().await;
        lane.store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn freeze_supersede_retire_route_and_forget_peer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lane = Arc::new(RealLaneWrap::new(&dir));
        let checkpoints = RecoveryCheckpoints::default();
        let r1 = route("se-1", "adm-1");
        let cp1 = checkpoints.freeze(&r1, lane.clone(), false).expect("cp1");
        assert_eq!(cp1.token.admission.generation, 1);
        assert_eq!(checkpoints.open(), 1);

        // Join reuses live cut
        let joined = checkpoints
            .freeze(&route("se-1", "adm-2"), lane.clone(), true)
            .expect("join");
        assert_eq!(joined.token.id, cp1.token.id);

        // Next freeze supersedes cp1
        let cp2 = checkpoints.freeze(&r1, lane.clone(), false).expect("cp2");
        assert_eq!(cp2.token.admission.generation, 2);
        assert_eq!(
            checkpoints.check(&cp1.token, &r1.caller()),
            Err(crate::compact_recovery_types::superseded_token(2))
        );
        assert_eq!(checkpoints.status().reclaimed, 1);

        // Foreign caller gets unknown_token
        let foreign = RecoveryCaller {
            socket_epoch: "other".into(),
            session_id: "s1".into(),
        };
        assert_eq!(
            checkpoints.check(&cp2.token, &foreign),
            Err(repair_needed(RepairReason::UnknownToken))
        );

        // Catalog page works
        let page = checkpoints
            .read_catalog_page(&cp2.token, &r1.caller(), "manifest", 0, None)
            .await
            .expect("manifest page");
        assert!(page.complete);

        // Retiring last route reclaims cp2
        checkpoints.retire_route(&r1.caller(), "adm-2", RouteRetirement::ClientDetach);
        assert_eq!(checkpoints.open(), 1);
        checkpoints.retire_route(&r1.caller(), "adm-1", RouteRetirement::ClientDetach);
        assert_eq!(checkpoints.open(), 0);
        lane.store.idle().await;
        lane.store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn retain_outlives_token_release_until_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lane = Arc::new(RealLaneWrap::new(&dir));
        let checkpoints = RecoveryCheckpoints::default();
        let r1 = route("se-1", "adm-1");
        let cp1 = checkpoints.freeze(&r1, lane.clone(), false).expect("cp1");
        let mut retained = cp1.retain();
        checkpoints.release(&cp1.token);
        assert!(
            lane.released_keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
        retained.end();
        assert_eq!(
            lane.released_keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            1
        );
        lane.store.idle().await;
        lane.store.retire().await.expect("retire");
    }
}
