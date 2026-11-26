//! Port of senpi packages/ai/src/auth/oauth-refresh.ts.
//!
//! `CredentialStore.modify` runs its callback under the store's mutual exclusion (a file lock for
//! `auth.json`). Running the up-to-15s HTTP exchange inside it made one account's refresh block
//! every other account, provider, and login for longer than they wait. The exchange now runs
//! outside `modify`; the write re-acquires the lock and compare-and-swaps on the slot's refresh
//! token, so a slot rotated meanwhile by another process is adopted instead of overwritten.
//!
//! Persistent stores can guard the read/exchange/CAS with a separate session lock.
//! Within a process, concurrent refreshes of the same slot join one exchange. Waiters that own
//! the exchange (per-request resolution) cancel it once none of them is waiting; the
//! catalog-refresh lane joins without owning it, so a superseded catalog refresh stops waiting
//! but never aborts the exchange. The TS `WeakMap<CredentialStore, Map<string, InflightRefresh>>`
//! keys by store object identity; this port keys the equivalent global registry by the store's
//! data pointer, cleaned up when that store's last refresh and capture are gone.

use crate::auth::pool::slots::{
    DEFAULT_SLOT_NAME, PooledCredential, list_slots, merge_refreshed, merge_refreshed_slot,
    project_slot,
};
use crate::auth::types::{
    AuthOperationOptions, Credential, CredentialStore, OAuthAuth, OAuthCredential,
    OAuthRevocationResult,
};
use crate::utils::abort::{AbortController, AbortSignal, race_with_abort_signal};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Notify;

pub const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: u64 = 15_000;

pub struct OAuthRefreshRequest {
    pub credentials: Arc<dyn CredentialStore>,
    pub provider_id: String,
    pub oauth: Arc<dyn OAuthAuth>,
    /// The credential (or slot projection) the caller found stale.
    pub stale: OAuthCredential,
    pub slot_name: Option<String>,
    /// Re-evaluated against the latest stored value before the exchange.
    pub is_stale: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    pub signal: AbortSignal,
    /// An owning waiter's abort cancels the exchange once no waiter remains; a non-owning one
    /// only stops waiting.
    pub owning: bool,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RefreshError {
    #[error("OAuth token exchange failed: {0}")]
    Exchange(String),
    #[error("Credential store operation failed: {0}")]
    Store(String),
    #[error("{0}")]
    Aborted(String),
}

struct InflightRefresh {
    result: Mutex<Option<Result<Option<Credential>, RefreshError>>>,
    done: Notify,
    controller: AbortController,
    waiters: Mutex<RefreshWaiters>,
}

struct RefreshWaiters {
    count: usize,
    owned: bool,
    aborting: bool,
}

type CapturedRefreshes = Arc<Mutex<Vec<Arc<InflightRefresh>>>>;

#[derive(Default)]
struct StoreRegistry {
    inflight: HashMap<(String, Option<String>, String), Arc<InflightRefresh>>,
    retiring: Vec<(String, Arc<InflightRefresh>)>,
    captures: HashMap<String, Vec<CapturedRefreshes>>,
}

impl StoreRegistry {
    fn is_empty(&self) -> bool {
        self.inflight.is_empty() && self.retiring.is_empty() && self.captures.is_empty()
    }
}

static INFLIGHT_BY_STORE: OnceLock<Mutex<HashMap<usize, StoreRegistry>>> = OnceLock::new();

fn inflight_registry() -> &'static Mutex<HashMap<usize, StoreRegistry>> {
    INFLIGHT_BY_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn store_key(store: &dyn CredentialStore) -> usize {
    std::ptr::from_ref(store).cast::<()>() as usize
}

pub(crate) struct OAuthRefreshCapture {
    store_id: usize,
    provider_id: String,
    pending: CapturedRefreshes,
}

impl OAuthRefreshCapture {
    pub(crate) fn finish(self) -> impl Future<Output = bool> + Send + 'static {
        let pending = self.pending.clone();
        drop(self);
        let pending = std::mem::take(&mut *pending.lock().unwrap_or_else(|p| p.into_inner()));
        async move {
            let mut settled = true;
            for refresh in pending {
                settled &= refresh.wait().await.is_ok();
            }
            settled
        }
    }
}

impl Drop for OAuthRefreshCapture {
    fn drop(&mut self) {
        let mut registry = inflight_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(store) = registry.get_mut(&self.store_id) {
            if let Some(captures) = store.captures.get_mut(&self.provider_id) {
                captures.retain(|capture| !Arc::ptr_eq(capture, &self.pending));
                if captures.is_empty() {
                    store.captures.remove(&self.provider_id);
                }
            }
            if store.is_empty() {
                registry.remove(&self.store_id);
            }
        }
    }
}

/// Collect exchanges until the local clear publishes, including discarded-token cleanup.
pub(crate) fn capture_oauth_refreshes(
    credentials: &dyn CredentialStore,
    provider_id: &str,
) -> OAuthRefreshCapture {
    let store_id = store_key(credentials);
    let mut registry = inflight_registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let store = registry.entry(store_id).or_default();
    let pending = Arc::new(Mutex::new(
        store
            .inflight
            .iter()
            .filter(|((id, _, _), _)| id == provider_id)
            .map(|(_, refresh)| refresh.clone())
            .chain(
                store
                    .retiring
                    .iter()
                    .filter(|(id, _)| id == provider_id)
                    .map(|(_, refresh)| refresh.clone()),
            )
            .collect(),
    ));
    store
        .captures
        .entry(provider_id.to_owned())
        .or_default()
        .push(pending.clone());
    OAuthRefreshCapture {
        store_id,
        provider_id: provider_id.to_owned(),
        pending,
    }
}

impl InflightRefresh {
    async fn wait(&self) -> Result<Option<Credential>, RefreshError> {
        loop {
            let mut notified = std::pin::pin!(self.done.notified());
            notified.as_mut().enable();
            if let Some(result) = self
                .result
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}

struct RefreshWaiter(Arc<InflightRefresh>);

impl Drop for RefreshWaiter {
    fn drop(&mut self) {
        let abort = {
            let mut waiters = self.0.waiters.lock().unwrap_or_else(|p| p.into_inner());
            waiters.count -= 1;
            let abort = waiters.count == 0 && waiters.owned;
            waiters.aborting |= abort;
            abort
        };
        if abort {
            self.0.controller.abort(None);
        }
    }
}

pub fn project_oauth_slot(credential: &PooledCredential, name: &str) -> Option<OAuthCredential> {
    let projected = project_slot(Some(credential), name)?;
    projected.into_oauth()
}

/// Resolves with the post-refresh stored credential (the whole provider entry), the newer stored
/// value when another writer rotated the slot first, or `None` when the credential or slot is
/// gone.
pub async fn refresh_oauth_credential(
    request: OAuthRefreshRequest,
) -> Result<Option<Credential>, RefreshError> {
    let store_id = store_key(request.credentials.as_ref());
    let key = (
        request.provider_id.clone(),
        request.slot_name.clone(),
        request.stale.refresh.clone(),
    );

    let (inflight, started) = {
        let mut registry = inflight_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let store = registry.entry(store_id).or_default();
        let existing = store.inflight.get(&key).cloned().filter(|inflight| {
            let mut waiters = inflight.waiters.lock().unwrap_or_else(|p| p.into_inner());
            if waiters.aborting {
                return false;
            }
            waiters.count += 1;
            waiters.owned |= request.owning;
            true
        });
        if let Some(inflight) = existing {
            (inflight, false)
        } else {
            let inflight = Arc::new(InflightRefresh {
                result: Mutex::new(None),
                done: Notify::new(),
                controller: AbortController::new(),
                waiters: Mutex::new(RefreshWaiters {
                    count: 1,
                    owned: request.owning,
                    aborting: false,
                }),
            });
            if let Some(previous) = store.inflight.insert(key.clone(), inflight.clone()) {
                store.retiring.push((request.provider_id.clone(), previous));
            }
            if let Some(captures) = store.captures.get(&request.provider_id) {
                for capture in captures {
                    capture
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(inflight.clone());
                }
            }
            (inflight, true)
        }
    };
    let _waiter = RefreshWaiter(inflight.clone());

    let signal = request.signal.clone();
    if started {
        let inflight_clone = inflight.clone();
        let exchange_signal = inflight.controller.signal();
        tokio::spawn(async move {
            let result = serialize_exchange(&request, exchange_signal).await;
            *inflight_clone
                .result
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(result);
            inflight_clone.done.notify_waiters();
            let mut registry = inflight_registry()
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(store_registry) = registry.get_mut(&store_id) {
                if store_registry
                    .inflight
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &inflight_clone))
                {
                    store_registry.inflight.remove(&key);
                }
                store_registry
                    .retiring
                    .retain(|(_, refresh)| !Arc::ptr_eq(refresh, &inflight_clone));
                if store_registry.is_empty() {
                    registry.remove(&store_id);
                }
            }
        });
    }

    match race_with_abort_signal(inflight.wait(), &signal).await {
        Ok(result) => result,
        Err(reason) => Err(RefreshError::Aborted(reason.to_string())),
    }
}

fn refresh_slot_name(credential: &OAuthCredential, slot_name: Option<&str>) -> String {
    if let Some(name) = slot_name {
        return name.to_owned();
    }
    let pooled = PooledCredential::from(Credential::OAuth(credential.clone()));
    list_slots(Some(&pooled))
        .into_iter()
        .find(|slot| {
            slot.refresh.as_deref() == Some(credential.refresh.as_str())
                || slot.access.as_deref() == Some(credential.access.as_str())
        })
        .map_or_else(|| DEFAULT_SLOT_NAME.to_owned(), |slot| slot.name)
}

async fn serialize_exchange(
    request: &OAuthRefreshRequest,
    signal: AbortSignal,
) -> Result<Option<Credential>, RefreshError> {
    let mut identity = refresh_slot_name(&request.stale, request.slot_name.as_deref());
    loop {
        let credentials = request.credentials.clone();
        let provider_id = request.provider_id.clone();
        let oauth = request.oauth.clone();
        let slot_name = request.slot_name.clone();
        let is_stale = request.is_stale.clone();
        let next_identity = Arc::new(Mutex::new(None));
        let retry = next_identity.clone();
        let locked_identity = identity.clone();
        let result = request
            .credentials
            .with_refresh_lock(
                &request.provider_id,
                &identity,
                Box::new(move |lock_signal| {
                    Box::pin(async move {
                        let latest = credentials
                            .read(
                                &provider_id,
                                Some(AuthOperationOptions {
                                    signal: Some(lock_signal.clone()),
                                }),
                            )
                            .await
                            .map_err(|error| RefreshError::Store(error.to_string()))?;
                        if let Some(current) = latest.as_ref().and_then(Credential::as_oauth) {
                            let current_identity = refresh_slot_name(current, slot_name.as_deref());
                            if locked_identity != current_identity {
                                // A flat mirror moved while waiting. Release before locking its new slot.
                                *retry.lock().unwrap_or_else(|p| p.into_inner()) =
                                    Some(current_identity);
                                return Ok(None);
                            }
                        }
                        exchange_and_store(
                            &credentials,
                            &provider_id,
                            &oauth,
                            slot_name.as_deref(),
                            &is_stale,
                            &lock_signal,
                            latest,
                        )
                        .await
                    })
                }),
                Some(AuthOperationOptions {
                    signal: Some(signal.clone()),
                }),
            )
            .await?;
        let next = next_identity
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        match next {
            Some(next) => identity = next,
            None => return Ok(result),
        }
    }
}

fn refresh_view(
    credential: Option<&Credential>,
    slot_name: Option<&str>,
) -> Option<OAuthCredential> {
    let credential = credential?;
    let oauth = credential.as_oauth()?;
    let projected = match slot_name {
        None => oauth.clone(),
        Some(name) => project_oauth_slot(&PooledCredential::from(credential.clone()), name)?,
    };
    (!(projected.is_signed_out() || (projected.access.is_empty() && projected.refresh.is_empty())))
        .then_some(projected)
}

async fn exchange_and_store(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &Arc<dyn OAuthAuth>,
    slot_name: Option<&str>,
    is_stale: &Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    signal: &AbortSignal,
    latest: Option<Credential>,
) -> Result<Option<Credential>, RefreshError> {
    let current = match refresh_view(latest.as_ref(), slot_name) {
        Some(current) => current,
        None => return Ok(None),
    };
    if !is_stale(&current) {
        return Ok(latest);
    }

    let exchange_signal = signal.clone();
    let exchanged = tokio::time::timeout(
        std::time::Duration::from_millis(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS),
        oauth.refresh(&current, &exchange_signal),
    )
    .await;
    let refreshed = match exchanged {
        Ok(result) => result.map_err(|e| RefreshError::Exchange(e.to_string()))?,
        Err(_elapsed) => return Err(RefreshError::Exchange("timed out".into())),
    };
    let can_revoke = oauth.has_revoke();
    if !can_revoke {
        signal
            .throw_if_aborted()
            .map_err(|reason| RefreshError::Aborted(reason.to_string()))?;
    }
    let discarded = Arc::new(AtomicBool::new(false));
    let stored = credentials
        .modify(
            provider_id,
            Box::new({
                let slot_name = slot_name.map(str::to_owned);
                let current_refresh = current.refresh.clone();
                let refreshed_credential = Credential::OAuth(refreshed.clone());
                let discarded = discarded.clone();
                let signal = signal.clone();
                move |stored: Option<Credential>| {
                    Box::pin(async move {
                        let slot = refresh_view(stored.as_ref(), slot_name.as_deref());
                        if slot.is_none_or(|slot| slot.refresh != current_refresh)
                            || (can_revoke && signal.aborted())
                        {
                            discarded.store(true, Ordering::Relaxed);
                            return Ok(None);
                        }
                        let Some(stored) = stored else {
                            return Ok(None);
                        };
                        let pooled: PooledCredential = stored.into();
                        let merged = match &slot_name {
                            None => merge_refreshed(&pooled, &refreshed_credential),
                            Some(name) => {
                                merge_refreshed_slot(&pooled, name, &refreshed_credential)
                            }
                        };
                        Ok(Some(merged.to_stored_credential()))
                    })
                }
            }),
            None,
        )
        .await
        .map_err(|e| RefreshError::Store(e.to_string()))?;
    let view = refresh_view(stored.as_ref(), slot_name);
    if discarded.load(Ordering::Relaxed)
        && can_revoke
        && view
            .as_ref()
            .is_none_or(|view| view.refresh != refreshed.refresh)
    {
        let mut target = refreshed;
        let mut extra = current.extra;
        extra.append(&mut target.extra);
        target.extra = extra;
        if !matches!(
            oauth.revoke(&target, None).await,
            Ok(OAuthRevocationResult::Revoked)
        ) {
            return Err(RefreshError::Exchange(
                "Discarded refresh revocation was not confirmed".into(),
            ));
        }
    }
    Ok(view.and(stored))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::{InMemoryCredentialStore, revoke_stored_oauth};
    use crate::auth::types::{
        CredentialInfo, LogoutOptions, ModelAuth, ProviderAuthInteraction, RefreshLockOperation,
    };
    use async_trait::async_trait;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    type Mutation = Box<
        dyn FnOnce(
                Option<Credential>,
            )
                -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Credential>>> + Send>>
            + Send,
    >;
    type PublicationGate = (
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    );

    #[derive(Default)]
    struct GateStore {
        inner: InMemoryCredentialStore,
        lock: tokio::sync::Mutex<()>,
        attempts: Mutex<Vec<String>>,
        attempted: Notify,
        pause_next_modify: Mutex<Option<PublicationGate>>,
    }

    #[async_trait]
    impl CredentialStore for GateStore {
        async fn read(
            &self,
            id: &str,
            options: Option<AuthOperationOptions>,
        ) -> anyhow::Result<Option<Credential>> {
            self.inner.read(id, options).await
        }

        async fn list(
            &self,
            options: Option<AuthOperationOptions>,
        ) -> anyhow::Result<Vec<CredentialInfo>> {
            self.inner.list(options).await
        }

        async fn modify(
            &self,
            id: &str,
            f: Mutation,
            options: Option<AuthOperationOptions>,
        ) -> anyhow::Result<Option<Credential>> {
            let pause = self.pause_next_modify.lock().unwrap().take();
            self.inner
                .modify(
                    id,
                    Box::new(move |current| {
                        Box::pin(async move {
                            let next = f(current).await?;
                            if let Some((prepared, publish)) = pause {
                                prepared.send(()).unwrap();
                                publish.await.unwrap();
                            }
                            Ok(next)
                        })
                    }),
                    options,
                )
                .await
        }

        async fn modify_or_delete(
            &self,
            id: &str,
            f: Mutation,
            options: Option<AuthOperationOptions>,
        ) -> anyhow::Result<Option<Credential>> {
            self.inner.modify_or_delete(id, f, options).await
        }

        async fn delete(
            &self,
            id: &str,
            options: Option<AuthOperationOptions>,
        ) -> anyhow::Result<()> {
            self.inner.delete(id, options).await
        }

        async fn with_refresh_lock(
            &self,
            _: &str,
            slot: &str,
            run: RefreshLockOperation,
            options: Option<AuthOperationOptions>,
        ) -> Result<Option<Credential>, RefreshError> {
            self.attempts.lock().unwrap().push(slot.to_owned());
            self.attempted.notify_one();
            let signal =
                crate::utils::abort::operation_signal(options.and_then(|opts| opts.signal));
            let _guard = race_with_abort_signal(self.lock.lock(), &signal)
                .await
                .map_err(|error| RefreshError::Store(error.to_string()))?;
            run(signal).await
        }
    }

    #[derive(Default)]
    struct ControlledOAuth {
        seen: Mutex<Vec<OAuthCredential>>,
        revoked: Mutex<Vec<String>>,
        revoking: Notify,
        started: Notify,
        release: Option<Notify>,
        release_revoke: Option<Notify>,
        unconfirmed: bool,
    }

    #[async_trait]
    impl OAuthAuth for ControlledOAuth {
        fn name(&self) -> &str {
            "fake"
        }

        async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            unreachable!("fake refresh only")
        }

        async fn refresh(
            &self,
            credential: &OAuthCredential,
            _: &AbortSignal,
        ) -> anyhow::Result<OAuthCredential> {
            self.seen.lock().unwrap().push(credential.clone());
            self.started.notify_one();
            if let Some(release) = &self.release {
                release.notified().await;
            }
            let mut next = credential.clone();
            next.access = "rotated-a".into();
            next.refresh = "rotated-r".into();
            next.expires = 999_999_999_999.0;
            Ok(next.with_extra(
                "scopes",
                serde_json::json!(["resource.invoke", "chatgpt.tokens.use.direct"]),
            ))
        }

        async fn to_auth(&self, _: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            unreachable!("fake refresh only")
        }

        fn has_revoke(&self) -> bool {
            true
        }

        async fn revoke(
            &self,
            credential: &OAuthCredential,
            _: Option<LogoutOptions>,
        ) -> anyhow::Result<OAuthRevocationResult> {
            self.revoked
                .lock()
                .unwrap()
                .push(credential.refresh.clone());
            self.revoking.notify_one();
            if let Some(release) = &self.release_revoke {
                release.notified().await;
            }
            Ok(if self.unconfirmed && credential.refresh == "rotated-r" {
                OAuthRevocationResult::RemoteRevocationUnconfirmed
            } else {
                OAuthRevocationResult::Revoked
            })
        }
    }

    async fn seed(store: &dyn CredentialStore, input: serde_json::Value) -> Credential {
        let credential = serde_json::from_value(input).unwrap();
        store
            .modify(
                "p",
                Box::new(|_| Box::pin(async move { Ok(Some(credential)) })),
                None,
            )
            .await
            .unwrap()
            .unwrap()
    }

    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("test gate timed out")
    }

    fn request(
        store: Arc<dyn CredentialStore>,
        oauth: Arc<dyn OAuthAuth>,
        stale: OAuthCredential,
        slot: Option<&str>,
    ) -> OAuthRefreshRequest {
        OAuthRefreshRequest {
            credentials: store,
            provider_id: "p".into(),
            oauth,
            stale,
            slot_name: slot.map(str::to_owned),
            is_stale: Arc::new(|token| token.expires <= 1.0),
            signal: AbortController::new().signal(),
            owning: true,
        }
    }

    #[tokio::test]
    async fn refreshes_selected_siwc_registration_and_merges_scopes_without_changing_siblings() {
        let store = Arc::new(GateStore::default());
        let oauth = Arc::new(ControlledOAuth::default());
        let original = seed(store.as_ref(), serde_json::json!({
            "type": "oauth", "expires": 30.0,
            "accounts": [
                { "name": "default", "access": "first-a", "refresh": "first-r", "expires": 30.0,
                  "route": "siwc", "clientId": "oaiapp_first" },
                { "name": "login-2", "access": "second-a", "refresh": "second-r", "expires": 0.0,
                  "route": "siwc", "issuer": "synthetic-issuer", "subject": "second",
                  "clientId": "oaiapp_second", "idToken": "synthetic-id-second", "loginId": "stable-second",
                  "scopes": ["old"] }
            ]
        })).await;
        let original_flat = original.as_oauth().unwrap();
        assert!(
            refresh_oauth_credential(request(
                store.clone(),
                oauth.clone(),
                original_flat.clone(),
                None
            ))
            .await
            .unwrap()
            .is_none()
        );
        assert!(oauth.seen.lock().unwrap().is_empty());
        let stale = project_oauth_slot(&original.clone().into(), "login-2").unwrap();
        let saved = refresh_oauth_credential(request(
            store.clone(),
            oauth.clone(),
            stale,
            Some("login-2"),
        ))
        .await
        .unwrap()
        .unwrap();
        let seen = oauth.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].get_extra_str("route"), Some("siwc"));
        assert_eq!(seen[0].get_extra_str("clientId"), Some("oaiapp_second"));
        assert_eq!(seen[0].refresh, "second-r");
        let accounts = saved.as_oauth().unwrap().get_extra("accounts").unwrap();
        assert_eq!(accounts[0], original_flat.get_extra("accounts").unwrap()[0]);
        assert_eq!(accounts[1]["refresh"], "rotated-r");
        assert_eq!(accounts[1]["idToken"], "synthetic-id-second");
        assert_eq!(accounts[1]["loginId"], "stable-second");
        assert_eq!(
            accounts[1]["scopes"],
            serde_json::json!(["resource.invoke", "chatgpt.tokens.use.direct"])
        );
    }

    #[tokio::test]
    async fn flat_and_named_refresh_hold_the_same_slot_lock_through_cas() {
        let store = Arc::new(GateStore::default());
        let oauth = Arc::new(ControlledOAuth {
            release: Some(Notify::new()),
            ..Default::default()
        });
        let original = seed(store.as_ref(), serde_json::json!({
            "type": "oauth", "access": "old-a", "refresh": "old-r", "expires": 0.0,
            "accounts": [{ "name": "work", "access": "old-a", "refresh": "old-r", "expires": 0.0 }]
        })).await;
        let flat = original.as_oauth().unwrap().clone();
        let named = project_oauth_slot(&original.into(), "work").unwrap();
        let a = tokio::spawn(refresh_oauth_credential(request(
            store.clone(),
            oauth.clone(),
            flat,
            None,
        )));
        bounded(oauth.started.notified()).await;
        bounded(store.attempted.notified()).await;
        let b = tokio::spawn(refresh_oauth_credential(request(
            store.clone(),
            oauth.clone(),
            named,
            Some("work"),
        )));
        bounded(store.attempted.notified()).await;
        assert!(store.lock.try_lock().is_err());
        assert_eq!(store.attempts.lock().unwrap().as_slice(), ["work", "work"]);
        oauth.release.as_ref().unwrap().notify_one();
        let a = bounded(a).await.unwrap().unwrap();
        let b = bounded(b).await.unwrap().unwrap();
        assert_eq!(a, b);
        assert_eq!(oauth.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn includes_refresh_admitted_while_store_is_publishing_local_clear() {
        let store = Arc::new(GateStore::default());
        let oauth = Arc::new(ControlledOAuth {
            release: Some(Notify::new()),
            unconfirmed: true,
            ..Default::default()
        });
        let original = seed(
            store.as_ref(),
            serde_json::json!({
                "type": "oauth", "access": "old-a", "refresh": "old-r", "expires": 0.0
            }),
        )
        .await
        .into_oauth()
        .unwrap();
        let (prepared_tx, prepared_rx) = tokio::sync::oneshot::channel();
        let (publish_tx, publish_rx) = tokio::sync::oneshot::channel();
        *store.pause_next_modify.lock().unwrap() = Some((prepared_tx, publish_rx));
        let logout = tokio::spawn({
            let store = store.clone();
            let oauth = oauth.clone();
            async move { revoke_stored_oauth(store.as_ref(), "p", oauth.as_ref(), None).await }
        });
        bounded(prepared_rx).await.unwrap();
        let refresh = tokio::spawn(refresh_oauth_credential(request(
            store.clone(),
            oauth.clone(),
            original,
            None,
        )));
        // The exchange must start even though the mutation lock is held by local clear.
        bounded(oauth.started.notified()).await;
        publish_tx.send(()).unwrap();
        bounded(oauth.revoking.notified()).await;
        oauth.release.as_ref().unwrap().notify_one();
        assert!(matches!(
            bounded(refresh).await.unwrap(),
            Err(RefreshError::Exchange(_))
        ));
        assert_eq!(
            bounded(logout).await.unwrap().unwrap(),
            Some(OAuthRevocationResult::RemoteRevocationUnconfirmed)
        );
        assert_eq!(
            oauth.revoked.lock().unwrap().as_slice(),
            ["old-r", "rotated-r"]
        );
        let stored = store
            .read("p", None)
            .await
            .unwrap()
            .unwrap()
            .into_oauth()
            .unwrap();
        assert!(stored.is_signed_out());
        assert!(stored.refresh.is_empty());
    }

    #[tokio::test]
    async fn local_clear_precedes_revocation_and_does_not_delete_a_concurrent_login() {
        let store = Arc::new(GateStore::default());
        let oauth = Arc::new(ControlledOAuth {
            release_revoke: Some(Notify::new()),
            ..Default::default()
        });
        let stale_read = seed(
            store.as_ref(),
            serde_json::json!({
                "type": "oauth", "access": "old-a", "refresh": "old-r", "expires": 0.0
            }),
        )
        .await
        .into_oauth()
        .unwrap();
        let logout = tokio::spawn({
            let store = store.clone();
            let oauth = oauth.clone();
            async move { revoke_stored_oauth(store.as_ref(), "p", oauth.as_ref(), None).await }
        });
        bounded(oauth.revoking.notified()).await;
        assert!(
            bounded(refresh_oauth_credential(request(
                store.clone(),
                oauth.clone(),
                stale_read,
                None
            )))
            .await
            .unwrap()
            .is_none()
        );
        assert!(oauth.seen.lock().unwrap().is_empty());
        let login = seed(
            store.as_ref(),
            serde_json::json!({
                "type": "oauth", "access": "login-a", "refresh": "login-r", "expires": 800.0
            }),
        )
        .await;
        oauth.release_revoke.as_ref().unwrap().notify_one();
        assert_eq!(
            bounded(logout).await.unwrap().unwrap(),
            Some(OAuthRevocationResult::Revoked)
        );
        assert_eq!(store.read("p", None).await.unwrap(), Some(login));
    }

    #[tokio::test]
    async fn adopts_concurrent_rotation_without_revoking_the_token_already_stored() {
        let store = Arc::new(GateStore::default());
        let oauth = Arc::new(ControlledOAuth {
            release: Some(Notify::new()),
            ..Default::default()
        });
        let old = seed(
            store.as_ref(),
            serde_json::json!({
                "type": "oauth", "access": "old-a", "refresh": "old-r", "expires": 0.0
            }),
        )
        .await
        .into_oauth()
        .unwrap();
        let refresh = tokio::spawn(refresh_oauth_credential(request(
            store.clone(),
            oauth.clone(),
            old,
            None,
        )));
        bounded(oauth.started.notified()).await;
        let written = seed(
            store.as_ref(),
            serde_json::json!({
                "type": "oauth", "access": "new-writer-a", "refresh": "rotated-r", "expires": 800.0
            }),
        )
        .await;
        oauth.release.as_ref().unwrap().notify_one();
        assert_eq!(bounded(refresh).await.unwrap().unwrap(), Some(written));
        assert!(oauth.revoked.lock().unwrap().is_empty());
    }

    struct FauxOAuth {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl OAuthAuth for FauxOAuth {
        fn name(&self) -> &str {
            "faux"
        }
        async fn login(
            &self,
            _interaction: &ProviderAuthInteraction,
        ) -> anyhow::Result<OAuthCredential> {
            unimplemented!()
        }
        async fn refresh(
            &self,
            credential: &OAuthCredential,
            _signal: &AbortSignal,
        ) -> anyhow::Result<OAuthCredential> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(OAuthCredential::new(
                format!("new-{}", credential.access),
                "new-refresh",
                999_999_999_999.0,
            ))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn refreshes_stale_credential_and_persists_result() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        store
            .modify(
                "p",
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new(
                            "old", "r1", 1.0,
                        ))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth {
            calls: calls.clone(),
        });
        let request = OAuthRefreshRequest {
            credentials: store.clone(),
            provider_id: "p".into(),
            oauth,
            stale: OAuthCredential::new("old", "r1", 1.0),
            slot_name: None,
            is_stale: Arc::new(|_| true),
            signal: AbortController::new().signal(),
            owning: true,
        };
        let result = refresh_oauth_credential(request).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn returns_none_when_credential_logged_out_meanwhile() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth {
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let request = OAuthRefreshRequest {
            credentials: store,
            provider_id: "missing".into(),
            oauth,
            stale: OAuthCredential::new("old", "r1", 1.0),
            slot_name: None,
            is_stale: Arc::new(|_| true),
            signal: AbortController::new().signal(),
            owning: true,
        };
        let result = refresh_oauth_credential(request).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn returns_latest_without_exchange_when_no_longer_stale() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        store
            .modify(
                "p",
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new(
                            "fresh",
                            "r1",
                            999_999_999_999.0,
                        ))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth {
            calls: calls.clone(),
        });
        let request = OAuthRefreshRequest {
            credentials: store,
            provider_id: "p".into(),
            oauth,
            stale: OAuthCredential::new("fresh", "r1", 999_999_999_999.0),
            slot_name: None,
            is_stale: Arc::new(|_| false),
            signal: AbortController::new().signal(),
            owning: true,
        };
        refresh_oauth_credential(request).await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "exchange must not run when not stale"
        );
    }

    #[tokio::test]
    async fn concurrent_refreshes_of_same_slot_join_one_exchange() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        store
            .modify(
                "p",
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new(
                            "old", "shared-r", 1.0,
                        ))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth {
            calls: calls.clone(),
        });
        let make_request = || OAuthRefreshRequest {
            credentials: store.clone(),
            provider_id: "p".into(),
            oauth: oauth.clone(),
            stale: OAuthCredential::new("old", "shared-r", 1.0),
            slot_name: None,
            is_stale: Arc::new(|_| true),
            signal: AbortController::new().signal(),
            owning: true,
        };
        let (a, b) = tokio::join!(
            refresh_oauth_credential(make_request()),
            refresh_oauth_credential(make_request())
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "concurrent refreshes of the same slot must join one exchange"
        );
    }

    #[tokio::test]
    async fn new_owner_does_not_join_an_aborted_exchange() {
        let store = Arc::new(GateStore::default());
        seed(
            store.as_ref(),
            serde_json::json!({"type":"oauth","access":"old","refresh":"r1","expires":0.0}),
        )
        .await;
        let oauth = Arc::new(ControlledOAuth {
            release: Some(Notify::new()),
            ..Default::default()
        });
        let controller = AbortController::new();
        let mut first = request(
            store.clone(),
            oauth.clone(),
            OAuthCredential::new("old", "r1", 0.0),
            None,
        );
        first.signal = controller.signal();
        let first = tokio::spawn(refresh_oauth_credential(first));
        bounded(store.attempted.notified()).await;
        bounded(oauth.started.notified()).await;
        controller.abort(None);
        assert!(bounded(first).await.unwrap().is_err());

        let second_attempt = store.attempted.notified();
        tokio::pin!(second_attempt);
        second_attempt.as_mut().enable();
        let second = tokio::spawn(refresh_oauth_credential(request(
            store.clone(),
            oauth.clone(),
            OAuthCredential::new("old", "r1", 0.0),
            None,
        )));
        let attempt = tokio::time::timeout(Duration::from_secs(5), &mut second_attempt).await;
        let capture = capture_oauth_refreshes(store.as_ref(), "p");
        let captured = capture.pending.lock().unwrap().len();
        drop(capture);
        oauth.release.as_ref().unwrap().notify_one();
        if attempt.is_ok() {
            bounded(oauth.started.notified()).await;
            oauth.release.as_ref().unwrap().notify_one();
        }
        let result = bounded(second).await.unwrap();
        assert!(attempt.is_ok(), "a new exchange was admitted");
        assert_eq!(
            captured, 2,
            "logout captures both old and replacement exchanges"
        );
        assert!(result.is_ok());
        assert_eq!(oauth.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn oauth_refresh_exchange_error_display_matches_senpi_message() {
        let error = RefreshError::Exchange("boom".into());
        assert_eq!(error.to_string(), "OAuth token exchange failed: boom");
    }

    #[test]
    fn oauth_refresh_store_error_display_matches_senpi_message() {
        let error = RefreshError::Store("boom".into());
        assert_eq!(error.to_string(), "Credential store operation failed: boom");
    }
}
