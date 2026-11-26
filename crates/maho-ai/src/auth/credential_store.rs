//! Port of senpi packages/ai/src/auth/credential-store.ts.

use crate::auth::oauth_refresh::capture_oauth_refreshes;
use crate::auth::pool::slots::{PooledCredential, list_slots, project_flat_fields, project_slot};
use crate::auth::types::{
    AuthOperationOptions, Credential, CredentialInfo, CredentialStore, LogoutOptions, OAuthAuth,
    OAuthRevocationResult,
};
use crate::utils::abort::{operation_signal, race_with_abort_signal};
use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn clear_tokens(
    access: &mut Option<String>,
    refresh: &mut Option<String>,
    expires: &mut Option<f64>,
    extra: &mut serde_json::Map<String, serde_json::Value>,
) {
    *access = Some(String::new());
    *refresh = Some(String::new());
    *expires = Some(0.0);
    extra.remove("idToken");
    extra.insert("signedOut".to_owned(), true.into());
}

/// Capture and clear locally under the store's write lock before attempting remote revocation.
pub async fn revoke_stored_oauth(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    oauth: &dyn OAuthAuth,
    options: Option<LogoutOptions>,
) -> anyhow::Result<Option<OAuthRevocationResult>> {
    use OAuthRevocationResult::{RemoteRevocationUnconfirmed, Revoked};

    if !oauth.has_revoke() {
        return Ok(None);
    }
    let capture = capture_oauth_refreshes(credentials, provider_id);
    let targets = Arc::new(Mutex::new(None));
    let slot_id = options.as_ref().and_then(|options| options.slot_id.clone());
    let modified = credentials
        .modify(
            provider_id,
            Box::new({
                let captured = targets.clone();
                move |current| {
                    Box::pin(async move {
                        let Some(current @ Credential::OAuth(_)) = current else {
                            return Ok(None);
                        };
                        let mut next = PooledCredential::from(current);
                        let slots: Vec<_> = list_slots(Some(&next))
                            .into_iter()
                            .filter(|slot| slot_id.as_ref().is_none_or(|id| slot.name == *id))
                            .collect();
                        let mut tokens = Vec::with_capacity(slots.len());
                        for slot in &slots {
                            if let Some(Credential::OAuth(mut projected)) =
                                project_slot(Some(&next), &slot.name)
                            {
                                let mut fields = serde_json::to_value(slot)?;
                                if let Some(fields) = fields.as_object_mut() {
                                    for field in ["access", "refresh", "expires"] {
                                        fields.remove(field);
                                    }
                                    projected.extra.append(fields);
                                }
                                tokens.push(projected);
                            }
                        }
                        *captured.lock().unwrap_or_else(|p| p.into_inner()) = Some(tokens);
                        if slots.is_empty() {
                            return Ok(None);
                        }
                        if slot_id.is_none()
                            || slots.iter().any(|slot| {
                                slot.refresh == next.refresh && slot.access == next.access
                            })
                        {
                            clear_tokens(
                                &mut next.access,
                                &mut next.refresh,
                                &mut next.expires,
                                &mut next.extra,
                            );
                        }
                        if let Some(accounts) = next.accounts.as_mut() {
                            for slot in accounts.iter_mut() {
                                if slots.iter().any(|selected| selected.name == slot.name) {
                                    clear_tokens(
                                        &mut slot.access,
                                        &mut slot.refresh,
                                        &mut slot.expires,
                                        &mut slot.extra,
                                    );
                                }
                            }
                        }
                        if next
                            .extra
                            .get("signedOut")
                            .and_then(serde_json::Value::as_bool)
                            == Some(true)
                            && let Some(active) = next
                                .accounts
                                .as_deref()
                                .and_then(|accounts| {
                                    accounts.iter().find(|slot| {
                                        slot.access.as_ref().is_some_and(|s| !s.is_empty())
                                            && slot.refresh.as_ref().is_some_and(|s| !s.is_empty())
                                            && slot.expires.is_some()
                                    })
                                })
                                .cloned()
                        {
                            next.extra.remove("accountId");
                            next = project_flat_fields(&next, &active);
                            if let Some(login_id) = active.extra.get("loginId") {
                                next.extra.insert("loginId".into(), login_id.clone());
                            }
                            next.extra.insert("signedOut".to_owned(), false.into());
                        }
                        Ok(Some(next.to_stored_credential()))
                    })
                }
            }),
            None,
        )
        .await;
    // Close the capture only after the store has published the clear, not in its callback.
    let refreshes = capture.finish();
    modified?;
    let Some(targets) = targets.lock().unwrap_or_else(|p| p.into_inner()).take() else {
        return Ok(None);
    };
    let mut result = if targets.is_empty() {
        RemoteRevocationUnconfirmed
    } else {
        Revoked
    };
    for target in &targets {
        if target.refresh.is_empty()
            || !matches!(oauth.revoke(target, options.clone()).await, Ok(Revoked))
        {
            result = RemoteRevocationUnconfirmed;
        }
    }
    if !refreshes.await {
        result = RemoteRevocationUnconfirmed;
    }
    Ok(Some(result))
}

type RefreshLocks = Mutex<HashMap<(String, String), std::sync::Arc<AsyncMutex<()>>>>;

#[derive(Default)]
pub struct InMemoryCredentialStore {
    credentials: Mutex<HashMap<String, Credential>>,
    locks: Mutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    refresh_locks: RefreshLocks,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_for(&self, provider_id: &str) -> std::sync::Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        locks
            .entry(provider_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
            .clone()
    }

    async fn enqueue<'a, T: Send + 'a>(
        &'a self,
        provider_id: &str,
        task: impl Future<Output = T> + Send + 'a,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<T> {
        let signal = operation_signal(options.and_then(|o| o.signal));
        let lock = self.lock_for(provider_id);
        let guard = lock.lock().await;
        if let Err(reason) = signal.throw_if_aborted() {
            drop(guard);
            anyhow::bail!("{reason}");
        }
        let result = race_with_abort_signal(task, &signal).await;
        drop(guard);
        result.map_err(|reason| anyhow::anyhow!("{reason}"))
    }
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn with_refresh_lock(
        &self,
        provider_id: &str,
        slot_name: &str,
        operation: crate::auth::types::RefreshLockOperation,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>, crate::auth::oauth_refresh::RefreshError> {
        let signal = operation_signal(options.and_then(|options| options.signal));
        let lock = {
            let mut locks = self.refresh_locks.lock().unwrap_or_else(|p| p.into_inner());
            locks
                .entry((provider_id.to_owned(), slot_name.to_owned()))
                .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _guard = race_with_abort_signal(lock.lock(), &signal)
            .await
            .map_err(|reason| {
                crate::auth::oauth_refresh::RefreshError::Aborted(reason.to_string())
            })?;
        operation(signal).await
    }

    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        if let Some(opts) = &options
            && let Some(signal) = &opts.signal
        {
            signal
                .throw_if_aborted()
                .map_err(|reason| anyhow::anyhow!("{reason}"))?;
        }
        let credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
        Ok(credentials.get(provider_id).cloned())
    }

    async fn list(
        &self,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Vec<CredentialInfo>> {
        if let Some(opts) = &options
            && let Some(signal) = &opts.signal
        {
            signal
                .throw_if_aborted()
                .map_err(|reason| anyhow::anyhow!("{reason}"))?;
        }
        let credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
        Ok(credentials
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: credential.credential_type(),
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: Box<
            dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>>
                + Send,
        >,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        let signal_opts = options.clone();
        let credentials_lock = &self.credentials;
        let provider_id_owned = provider_id.to_string();
        let task = async move {
            let current = {
                let credentials = credentials_lock.lock().unwrap_or_else(|p| p.into_inner());
                credentials.get(&provider_id_owned).cloned()
            };
            let next = f(current.clone()).await?;
            if let Some(next) = &next {
                let mut credentials = credentials_lock.lock().unwrap_or_else(|p| p.into_inner());
                credentials.insert(provider_id_owned.clone(), next.clone());
            }
            Ok(next.or(current))
        };
        self.enqueue(provider_id, task, signal_opts).await?
    }

    async fn modify_or_delete(
        &self,
        provider_id: &str,
        f: Box<
            dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>>
                + Send,
        >,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        let provider_id_owned = provider_id.to_owned();
        let task = async move {
            let current = self
                .credentials
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&provider_id_owned)
                .cloned();
            let next = f(current).await?;
            let mut credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
            match &next {
                Some(credential) => {
                    credentials.insert(provider_id_owned, credential.clone());
                }
                None => {
                    credentials.remove(&provider_id_owned);
                }
            }
            Ok(next)
        };
        self.enqueue(provider_id, task, options).await?
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<()> {
        let provider_id_owned = provider_id.to_string();
        let credentials_lock = &self.credentials;
        let task = async move {
            let mut credentials = credentials_lock.lock().unwrap_or_else(|p| p.into_inner());
            credentials.remove(&provider_id_owned);
            Ok::<(), anyhow::Error>(())
        };
        self.enqueue(provider_id, task, options).await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{
        ApiKeyCredential, ModelAuth, OAuthCredential, ProviderAuthInteraction,
    };
    use crate::utils::abort::{AbortController, AbortSignal};

    #[tokio::test]
    async fn refresh_lock_serializes_operations_for_the_same_slot() {
        let store = InMemoryCredentialStore::new();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first = store.with_refresh_lock(
            "fixture",
            "work",
            Box::new(move |_| {
                Box::pin(async move {
                    entered.send(()).expect("first observer");
                    released.await.expect("release first");
                    Ok(None)
                })
            }),
            None,
        );
        tokio::pin!(first);
        assert!(futures::poll!(&mut first).is_pending());
        started.await.expect("first entered");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            store.with_refresh_lock(
                "fixture",
                "other",
                Box::new(|_| Box::pin(async { Ok(None) })),
                None,
            ),
        )
        .await
        .expect("different slot must not wait")
        .expect("different slot operation");
        let controller = AbortController::new();
        let cancelled = store.with_refresh_lock(
            "fixture",
            "work",
            Box::new(|_| Box::pin(async { panic!("cancelled operation must not execute") })),
            Some(AuthOperationOptions {
                signal: Some(controller.signal()),
            }),
        );
        tokio::pin!(cancelled);
        assert!(futures::poll!(&mut cancelled).is_pending());
        controller.abort(None);
        assert!(matches!(
            cancelled.await,
            Err(crate::auth::oauth_refresh::RefreshError::Aborted(_))
        ));
        let (second_entered, second_started) = tokio::sync::oneshot::channel();
        let second = store.with_refresh_lock(
            "fixture",
            "work",
            Box::new(move |_| {
                Box::pin(async move {
                    second_entered.send(()).expect("second observer");
                    Ok(None)
                })
            }),
            None,
        );
        tokio::pin!(second);
        assert!(
            futures::poll!(&mut second).is_pending(),
            "second operation must wait"
        );
        release.send(()).expect("release first");
        first.await.expect("first operation");
        second.await.expect("second operation");
        second_started.await.expect("second entered");
    }

    #[derive(Default)]
    struct RecordingRevocation(Mutex<Vec<OAuthCredential>>);

    #[async_trait]
    impl OAuthAuth for RecordingRevocation {
        fn name(&self) -> &str {
            "fake"
        }

        async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            unreachable!("not part of local clear")
        }

        async fn refresh(
            &self,
            _: &OAuthCredential,
            _: &AbortSignal,
        ) -> anyhow::Result<OAuthCredential> {
            unreachable!("not part of local clear")
        }

        async fn to_auth(&self, _: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            unreachable!("not part of local clear")
        }

        fn has_revoke(&self) -> bool {
            true
        }

        async fn revoke(
            &self,
            credential: &OAuthCredential,
            options: Option<LogoutOptions>,
        ) -> anyhow::Result<OAuthRevocationResult> {
            self.0.lock().unwrap().push(credential.clone());
            if options
                .and_then(|options| options.signal)
                .is_some_and(|signal| signal.aborted())
            {
                anyhow::bail!("cancelled");
            }
            Ok(OAuthRevocationResult::Revoked)
        }
    }

    async fn seed_oauth(store: &InMemoryCredentialStore, input: serde_json::Value) -> Credential {
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

    #[tokio::test]
    async fn revokes_issued_client_and_clears_local_tokens_even_when_cancelled() {
        let store = InMemoryCredentialStore::new();
        let oauth = RecordingRevocation::default();
        seed_oauth(
            &store,
            serde_json::json!({
                "type": "oauth", "access": "old-a", "refresh": "old-r", "expires": 20.0,
                "clientId": "issued", "subject": "one", "idToken": "synthetic-id"
            }),
        )
        .await;
        let controller = AbortController::new();
        controller.abort(None);
        let options = LogoutOptions {
            signal: Some(controller.signal()),
            ..Default::default()
        };
        assert_eq!(
            revoke_stored_oauth(&store, "p", &oauth, Some(options))
                .await
                .unwrap(),
            Some(OAuthRevocationResult::RemoteRevocationUnconfirmed)
        );
        let cleared = store
            .read("p", None)
            .await
            .unwrap()
            .unwrap()
            .into_oauth()
            .unwrap();
        assert!(cleared.is_signed_out());
        assert_eq!(
            (
                cleared.access.as_str(),
                cleared.refresh.as_str(),
                cleared.expires
            ),
            ("", "", 0.0)
        );
        assert_eq!(cleared.get_extra_str("clientId"), Some("issued"));
        assert_eq!(cleared.get_extra_str("subject"), Some("one"));
        assert!(cleared.get_extra("idToken").is_none());
        let targets = oauth.0.lock().unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].get_extra_str("clientId"), Some("issued"));
        assert_eq!(targets[0].refresh, "old-r");
    }

    #[tokio::test]
    async fn does_not_confirm_revocation_for_an_unknown_slot() {
        let store = InMemoryCredentialStore::new();
        let oauth = RecordingRevocation::default();
        let before = seed_oauth(
            &store,
            serde_json::json!({
                "type": "oauth", "access": "a", "refresh": "r", "expires": 4.0
            }),
        )
        .await;
        let options = LogoutOptions {
            slot_id: Some("missing".into()),
            ..Default::default()
        };
        assert_eq!(
            revoke_stored_oauth(&store, "p", &oauth, Some(options))
                .await
                .unwrap(),
            Some(OAuthRevocationResult::RemoteRevocationUnconfirmed)
        );
        assert_eq!(store.read("p", None).await.unwrap(), Some(before));
        assert!(oauth.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn clears_only_selected_registration_and_reprojects_legacy_survivor() {
        let store = InMemoryCredentialStore::new();
        let oauth = RecordingRevocation::default();
        let before = seed_oauth(&store, serde_json::json!({
            "type": "oauth", "access": "a", "refresh": "r", "expires": 4.0,
            "route": "siwc", "clientId": "one", "issuer": "issuer-one", "loginId": "one-id",
            "accounts": [
                { "name": "first", "access": "a", "refresh": "r", "expires": 4.0,
                  "route": "siwc", "clientId": "one", "issuer": "issuer-one", "loginId": "one-id", "idToken": "id-a" },
                { "name": "second", "access": "b", "refresh": "s", "expires": 5.0,
                  "loginId": "two-id" }
            ]
        })).await;
        let options = LogoutOptions {
            slot_id: Some("first".into()),
            ..Default::default()
        };
        assert_eq!(
            revoke_stored_oauth(&store, "p", &oauth, Some(options))
                .await
                .unwrap(),
            Some(OAuthRevocationResult::Revoked)
        );
        let post = store
            .read("p", None)
            .await
            .unwrap()
            .unwrap()
            .into_oauth()
            .unwrap();
        assert_eq!(post.refresh, "s");
        assert!(!post.is_signed_out());
        assert_eq!(post.get_extra_str("loginId"), Some("two-id"));
        for field in ["route", "clientId", "issuer", "idToken"] {
            assert!(post.get_extra(field).is_none(), "stale flat {field}");
        }
        let accounts = post.get_extra("accounts").unwrap();
        assert_eq!(
            accounts[1],
            before.as_oauth().unwrap().get_extra("accounts").unwrap()[1]
        );
        assert_eq!(accounts[0]["clientId"], "one");
        assert_eq!(accounts[0]["loginId"], "one-id");
        assert_eq!(accounts[0]["signedOut"], true);
        assert!(accounts[0].get("idToken").is_none());
        assert_eq!(
            oauth.0.lock().unwrap()[0].get_extra_str("clientId"),
            Some("one")
        );
    }

    #[tokio::test]
    async fn conditional_delete_serializes_with_a_concurrent_login() {
        // Given a conditional deletion holding the provider's mutation lock.
        let store = InMemoryCredentialStore::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let deletion = store.modify_or_delete(
            "p",
            Box::new(move |_| {
                Box::pin(async move {
                    entered_tx.send(()).expect("entered subscriber");
                    release_rx.await.expect("release");
                    Ok(None)
                })
            }),
            None,
        );
        tokio::pin!(deletion);
        assert!(futures::poll!(deletion.as_mut()).is_pending());
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
            .await
            .expect("entry deadline")
            .expect("entered");
        let login = store.modify(
            "p",
            Box::new(|current| {
                Box::pin(async move {
                    assert!(current.is_none());
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("new".into()),
                        ..Default::default()
                    })))
                })
            }),
            None,
        );
        tokio::pin!(login);
        assert!(futures::poll!(login.as_mut()).is_pending());
        // When deletion completes and the queued login obtains the same lock.
        release_tx.send(()).expect("release subscriber");
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), deletion)
                .await
                .expect("delete deadline")
                .expect("delete")
                .is_none()
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), login)
            .await
            .expect("login deadline")
            .expect("login");
        // Then no trailing delete can remove the new login.
        assert_eq!(
            store
                .read("p", None)
                .await
                .expect("read")
                .expect("credential")
                .as_api_key()
                .and_then(|c| c.key.as_deref()),
            Some("new")
        );
    }

    #[tokio::test]
    async fn conditional_delete_failure_leaves_existing_credential_unchanged() {
        // Given a stored credential.
        let store = InMemoryCredentialStore::new();
        let initial = Credential::ApiKey(ApiKeyCredential {
            key: Some("old".into()),
            ..Default::default()
        });
        let seed = initial.clone();
        store
            .modify(
                "p",
                Box::new(move |_| Box::pin(async move { Ok(Some(seed)) })),
                None,
            )
            .await
            .expect("seed");
        // When the conditional mutation fails.
        let result = store
            .modify_or_delete(
                "p",
                Box::new(|_| Box::pin(async { anyhow::bail!("failed mutation") })),
                None,
            )
            .await;
        // Then failure does not commit a deletion or replacement.
        assert!(result.is_err());
        assert_eq!(store.read("p", None).await.expect("read"), Some(initial));
    }

    fn box_fut<F>(f: F) -> BoxFuture<'static, anyhow::Result<Option<Credential>>>
    where
        F: Future<Output = anyhow::Result<Option<Credential>>> + Send + 'static,
    {
        Box::pin(f)
    }

    #[tokio::test]
    async fn read_resolves_none_for_missing_entry() {
        let store = InMemoryCredentialStore::new();
        assert_eq!(store.read("anthropic", None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn modify_writes_and_read_reflects_it() {
        let store = InMemoryCredentialStore::new();
        let written = store
            .modify(
                "anthropic",
                Box::new(|_current| {
                    box_fut(async {
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("k".into()),
                            env: None,
                            ..Default::default()
                        })))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            written
                .as_ref()
                .and_then(Credential::as_api_key)
                .and_then(|c| c.key.clone()),
            Some("k".into())
        );
        let read_back = store.read("anthropic", None).await.unwrap();
        assert_eq!(read_back, written);
    }

    #[tokio::test]
    async fn modify_returning_none_leaves_entry_unchanged() {
        let store = InMemoryCredentialStore::new();
        store
            .modify(
                "p",
                Box::new(|_| {
                    box_fut(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new("a", "r", 1.0))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let result = store
            .modify(
                "p",
                Box::new(|current| {
                    box_fut(async move {
                        Ok::<Option<Credential>, anyhow::Error>(None).map(|_| current)
                    })
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            result.and_then(|c| c.as_oauth().cloned()).map(|c| c.access),
            Some("a".into())
        );
    }

    #[tokio::test]
    async fn delete_removes_entry() {
        let store = InMemoryCredentialStore::new();
        store
            .modify(
                "p",
                Box::new(|_| {
                    box_fut(async {
                        Ok(Some(Credential::OAuth(OAuthCredential::new("a", "r", 1.0))))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        store.delete("p", None).await.unwrap();
        assert_eq!(store.read("p", None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_reports_metadata_without_secrets() {
        let store = InMemoryCredentialStore::new();
        store
            .modify(
                "p1",
                Box::new(|_| {
                    box_fut(async {
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("secret".into()),
                            env: None,
                            ..Default::default()
                        })))
                    })
                }),
                None,
            )
            .await
            .unwrap();
        let listed = store.list(None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].provider_id, "p1");
        assert_eq!(
            listed[0].credential_type,
            crate::auth::types::CredentialType::ApiKey
        );
    }

    #[tokio::test]
    async fn writes_are_serialized_per_provider() {
        let store = std::sync::Arc::new(InMemoryCredentialStore::new());
        let order = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        let (o1, o2) = (order.clone(), order.clone());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let s1 = store.clone();
        let h1 = tokio::spawn(async move {
            s1.modify(
                "p",
                Box::new(move |_| {
                    box_fut(async move {
                        entered_tx.send(()).unwrap();
                        release_rx.await.unwrap();
                        o1.lock().unwrap_or_else(|p| p.into_inner()).push(1);
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("1".into()),
                            env: None,
                            ..Default::default()
                        })))
                    })
                }),
                None,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        let mut second = Box::pin(store.modify(
            "p",
            Box::new(move |current| {
                box_fut(async move {
                    assert_eq!(
                        current.and_then(|c| c.as_api_key().and_then(|c| c.key.clone())),
                        Some("1".into())
                    );
                    o2.lock().unwrap_or_else(|p| p.into_inner()).push(2);
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("2".into()),
                        env: None,
                        ..Default::default()
                    })))
                })
            }),
            None,
        ));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            h1.await.unwrap().unwrap();
            second.await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(*order.lock().unwrap_or_else(|p| p.into_inner()), vec![1, 2]);
    }
}
