//! Port of senpi packages/ai/src/auth/oauth-refresh.ts.
//!
//! `CredentialStore.modify` runs its callback under the store's mutual exclusion (a file lock for
//! `auth.json`). Running the up-to-15s HTTP exchange inside it made one account's refresh block
//! every other account, provider, and login for longer than they wait. The exchange now runs
//! outside `modify`; the write re-acquires the lock and compare-and-swaps on the slot's refresh
//! token, so a slot rotated meanwhile by another process is adopted instead of overwritten.
//!
//! Within a process, concurrent refreshes of the same slot join one exchange. Waiters that own
//! the exchange (per-request resolution) cancel it once none of them is waiting; the
//! catalog-refresh lane joins without owning it, so a superseded catalog refresh stops waiting
//! but never aborts the exchange. The TS `WeakMap<CredentialStore, Map<string, InflightRefresh>>`
//! keys by store object identity; this port keys the equivalent global registry by the store's
//! `Arc` pointer address, cleaned up when the last registry entry referencing that store drops.

use crate::auth::pool::slots::{merge_refreshed, merge_refreshed_slot, project_slot, PooledCredential};use crate::auth::types::{Credential, CredentialStore, OAuthAuth, OAuthCredential};
use crate::utils::abort::{race_with_abort_signal, AbortController, AbortSignal};
use std::collections::HashMap;
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
    waiters: Mutex<u32>,
    owned: Mutex<bool>,
}

type StoreRegistry = HashMap<String, Arc<InflightRefresh>>;
static INFLIGHT_BY_STORE: OnceLock<Mutex<HashMap<usize, StoreRegistry>>> = OnceLock::new();

fn inflight_registry() -> &'static Mutex<HashMap<usize, StoreRegistry>> {
    INFLIGHT_BY_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn store_key(store: &Arc<dyn CredentialStore>) -> usize {
    Arc::as_ptr(store) as *const () as usize
}

pub fn project_oauth_slot(credential: &PooledCredential, name: &str) -> Option<OAuthCredential> {
    let projected = project_slot(Some(credential), name)?;
    projected.into_oauth()
}

/// Resolves with the post-refresh stored credential (the whole provider entry), the newer stored
/// value when another writer rotated the slot first, or `None` when the credential or slot is
/// gone.
pub async fn refresh_oauth_credential(request: OAuthRefreshRequest) -> Result<Option<Credential>, RefreshError> {
    let store_id = store_key(&request.credentials);
    let key = format!("{}\u{0}{}\u{0}{}", request.provider_id, request.slot_name.as_deref().unwrap_or(""), request.stale.refresh);

    let inflight = {
        let mut registry = inflight_registry().lock().unwrap_or_else(|p| p.into_inner());
        let store_registry = registry.entry(store_id).or_default();
        store_registry
            .entry(key.clone())
            .or_insert_with(|| {
                Arc::new(InflightRefresh {
                    result: Mutex::new(None),
                    done: Notify::new(),
                    controller: AbortController::new(),
                    waiters: Mutex::new(0),
                    owned: Mutex::new(false),
                })
            })
            .clone()
    };

    let started = {
        let mut waiters = inflight.waiters.lock().unwrap_or_else(|p| p.into_inner());
        let first = *waiters == 0 && inflight.result.lock().unwrap_or_else(|p| p.into_inner()).is_none();
        *waiters += 1;
        first
    };
    if request.owning {
        *inflight.owned.lock().unwrap_or_else(|p| p.into_inner()) = true;
    }

    if started {
        let inflight_clone = inflight.clone();
        let exchange_signal = inflight.controller.signal();
        let provider_id = request.provider_id.clone();
        let slot_name = request.slot_name.clone();
        let stale = request.stale.clone();
        let is_stale = request.is_stale.clone();
        let credentials = request.credentials.clone();
        let oauth = request.oauth.clone();
        tokio::spawn(async move {
            let result = exchange_and_store(&credentials, &provider_id, &oauth, &stale, slot_name.as_deref(), &is_stale, &exchange_signal)
                .await;
            *inflight_clone.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
            inflight_clone.done.notify_waiters();
            let mut registry = inflight_registry().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(store_registry) = registry.get_mut(&store_id) {
                store_registry.remove(&key);
                if store_registry.is_empty() {
                    registry.remove(&store_id);
                }
            }
        });
    }

    let leave = {
        let inflight = inflight.clone();
        move || {
            let mut waiters = inflight.waiters.lock().unwrap_or_else(|p| p.into_inner());
            *waiters = waiters.saturating_sub(1);
            if *waiters == 0 && *inflight.owned.lock().unwrap_or_else(|p| p.into_inner()) {
                inflight.controller.abort(None);
            }
        }
    };

    let wait_for_result = {
        let inflight = inflight.clone();
        async move {
            loop {
                if let Some(result) = inflight.result.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                    return result;
                }
                inflight.done.notified().await;
            }
        }
    };

    let outcome = race_with_abort_signal(wait_for_result, &request.signal).await;
    leave();
    match outcome {
        Ok(result) => result,
        Err(reason) => Err(RefreshError::Aborted(reason.to_string())),
    }
}

#[allow(clippy::too_many_arguments)]
async fn exchange_and_store(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &Arc<dyn OAuthAuth>,
    stale: &OAuthCredential,
    slot_name: Option<&str>,
    is_stale: &Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    signal: &AbortSignal,
) -> Result<Option<Credential>, RefreshError> {
    let _ = stale;
    let view = |credential: Option<&Credential>| -> Option<OAuthCredential> {
        let credential = credential?;
        let oauth_credential = credential.as_oauth()?.clone();
        match slot_name {
            None => Some(oauth_credential),
            Some(name) => {
                let pooled: PooledCredential = credential.clone().into();
                project_oauth_slot(&pooled, name)
            }
        }
    };

    let latest = credentials
        .read(provider_id, None)
        .await
        .map_err(|e| RefreshError::Store(e.to_string()))?;
    let current = match view(latest.as_ref()) {
        Some(current) => current,
        None => return Ok(None),
    };
    if !is_stale(&current) {
        return Ok(latest);
    }

    let exchange_signal = signal.clone();
    let refreshed = match tokio::time::timeout(
        std::time::Duration::from_millis(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS),
        oauth.refresh(&current, &exchange_signal),
    )
    .await
    {
        Ok(result) => result.map_err(|e| RefreshError::Exchange(e.to_string()))?,
        Err(_elapsed) => return Err(RefreshError::Exchange("timed out".into())),
    };
    signal.throw_if_aborted().map_err(|reason| RefreshError::Exchange(reason.to_string()))?;

    let refreshed_credential = Credential::OAuth(refreshed);
    let stored = credentials
        .modify(
            provider_id,
            Box::new({
                let slot_name = slot_name.map(str::to_owned);
                let current_refresh = current.refresh.clone();
                move |stored: Option<Credential>| {
                    Box::pin(async move {
                        let Some(stored) = stored else { return Ok(None) };
                        if stored.as_oauth().is_none() {
                            return Ok(None);
                        }
                        let pooled: PooledCredential = stored.clone().into();
                        let slot = match &slot_name {
                            None => stored.as_oauth().cloned(),
                            Some(name) => project_oauth_slot(&pooled, name),
                        };
                        let Some(slot) = slot else { return Ok(None) };
                        if slot.refresh != current_refresh {
                            return Ok(None);
                        }
                        let merged = match &slot_name {
                            None => merge_refreshed(&pooled, &refreshed_credential),
                            Some(name) => merge_refreshed_slot(&pooled, name, &refreshed_credential),
                        };
                        Ok(Some(merged.to_stored_credential()))
                    })
                }
            }),
            None,
        )
        .await
        .map_err(|e| RefreshError::Store(e.to_string()))?;
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{ModelAuth, ProviderAuthInteraction};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FauxOAuth {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl OAuthAuth for FauxOAuth {
        fn name(&self) -> &str {
            "faux"
        }
        async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            unimplemented!()
        }
        async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(OAuthCredential::new(format!("new-{}", credential.access), "new-refresh", 999_999_999_999.0))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    #[tokio::test]
    async fn refreshes_stale_credential_and_persists_result() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        store
            .modify(
                "p",
                Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("old", "r1", 1.0)))) })),
                None,
            )
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth { calls: calls.clone() });
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
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth { calls: Arc::new(AtomicUsize::new(0)) });
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
                Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("fresh", "r1", 999_999_999_999.0)))) })),
                None,
            )
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth { calls: calls.clone() });
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
        assert_eq!(calls.load(Ordering::SeqCst), 0, "exchange must not run when not stale");
    }

    #[tokio::test]
    async fn concurrent_refreshes_of_same_slot_join_one_exchange() {
        let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        store
            .modify("p", Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(OAuthCredential::new("old", "shared-r", 1.0)))) })), None)
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FauxOAuth { calls: calls.clone() });
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
        let (a, b) = tokio::join!(refresh_oauth_credential(make_request()), refresh_oauth_credential(make_request()));
        a.unwrap();
        b.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "concurrent refreshes of the same slot must join one exchange");
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
