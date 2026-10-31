//! Port of senpi `anthropic-subscription/auth-block-recovery.ts` (pin 903b8336).
//!
//! `AccountSlot.auth_recovery_grant` carries the grant (senpi#2926). Recovery rewrites the stored
//! account inside one store `modify`, so it reads and writes `authRecoveryGrant` on that stored JSON,
//! which is the same field the slot deserializes.
use crate::accounts::{AccountSlot, AccountSource};
use maho_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type RecoveryFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send>>;
/// A future that may borrow the caller (the `reload` callback).
pub type BorrowedFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;
/// `SlotRefresher`: redeems a refresh token for fresh material.
pub type SlotRefresher =
    Arc<dyn Fn(String, AbortSignal) -> RecoveryFuture<OAuthCredential> + Send + Sync>;

/// `authGrantDigest` (accounts.ts): the first 32 hex characters of the refresh token's SHA-256.
pub fn auth_grant_digest(refresh: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(refresh.as_bytes()));
    digest[..32].to_owned()
}

pub struct AuthBlockRecoveryInput<'a> {
    pub store: &'a dyn CredentialStore,
    pub provider_id: &'a str,
    pub accounts: Vec<AccountSlot>,
    pub refresher: SlotRefresher,
    pub signal: AbortSignal,
    /// True when the token endpoint rejected the grant itself, not when it was throttled or unavailable.
    pub is_grant_rejected: Arc<dyn Fn(&anyhow::Error) -> bool + Send + Sync>,
    pub reload: Box<dyn Fn() -> BorrowedFuture<'a, Vec<AccountSlot>> + Send + Sync + 'a>,
    /// Whether an account can serve this request now; when none can, every recoverable account is recovered.
    pub is_usable: Box<dyn Fn(&AccountSlot) -> bool + Send + Sync + 'a>,
    pub now: f64,
    /// The request error for a refresh that failed for a reason other than a rejected grant.
    pub transient_error: Box<dyn Fn(anyhow::Error) -> anyhow::Error + Send + Sync + 'a>,
}

fn recoverable_parts(
    block_reason: Option<&str>,
    env: bool,
    refresh: &str,
    grant: Option<&str>,
) -> bool {
    block_reason == Some("auth_error")
        && !env
        && !refresh.is_empty()
        && grant != Some(auth_grant_digest(refresh).as_str())
}

/// An auth-blocked slot with saved refresh material gets one recovery per grant: the grant is
/// redeemed once, and a rejected grant, or the grant a recovery produced, is never redeemed again
/// until new material arrives through a refresh rotation or a re-login (senpi#2926).
pub fn is_auth_block_recoverable(slot: &AccountSlot, auth_recovery_grant: Option<&str>) -> bool {
    recoverable_parts(
        slot.block_reason.as_deref(),
        slot.source == AccountSource::Env,
        &slot.refresh,
        auth_recovery_grant,
    )
}

/// [`is_auth_block_recoverable`] over a stored account object.
pub fn is_stored_slot_recoverable(stored: &Value) -> bool {
    recoverable_parts(
        stored.get("blockReason").and_then(Value::as_str),
        stored.get("source").and_then(Value::as_str) == Some("env"),
        stored
            .get("refresh")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        stored.get("authRecoveryGrant").and_then(Value::as_str),
    )
}

async fn stored_grants(
    store: &dyn CredentialStore,
    provider_id: &str,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut grants = std::collections::HashMap::new();
    if let Some(Credential::OAuth(credential)) = store.read(provider_id, None).await?
        && let Some(accounts) = credential.extra.get("accounts").and_then(Value::as_array)
    {
        for account in accounts {
            if let (Some(name), Some(grant)) = (
                account.get("name").and_then(Value::as_str),
                account.get("authRecoveryGrant").and_then(Value::as_str),
            ) {
                grants.insert(name.to_owned(), grant.to_owned());
            }
        }
    }
    Ok(grants)
}

async fn recover_slot(input: &AuthBlockRecoveryInput<'_>, name: &str) -> anyhow::Result<()> {
    let name = name.to_owned();
    let refresher = input.refresher.clone();
    let signal = input.signal.clone();
    let is_grant_rejected = input.is_grant_rejected.clone();
    input
        .store
        .modify(
            input.provider_id,
            Box::new(move |current| {
                Box::pin(async move {
                    let mut credential = match current {
                        Some(Credential::OAuth(credential)) => credential,
                        other => return Ok(other),
                    };
                    let Some(stored) = credential
                        .extra
                        .get("accounts")
                        .and_then(Value::as_array)
                        .and_then(|accounts| {
                            accounts.iter().find(|slot| {
                                slot.get("name").and_then(Value::as_str) == Some(name.as_str())
                            })
                        })
                        .cloned()
                    else {
                        return Ok(Some(Credential::OAuth(credential)));
                    };
                    if !is_stored_slot_recoverable(&stored) {
                        return Ok(Some(Credential::OAuth(credential)));
                    }
                    let refresh = stored["refresh"].as_str().unwrap_or_default().to_owned();
                    let mut next = stored.clone();
                    let object = next.as_object_mut().expect("stored account object");
                    match refresher(refresh.clone(), signal.clone()).await {
                        Ok(refreshed) => {
                            object.remove("blockReason");
                            object.remove("blockedUntil");
                            object.insert("access".into(), Value::String(refreshed.access));
                            object
                                .insert("refresh".into(), Value::String(refreshed.refresh.clone()));
                            object.insert("expires".into(), serde_json::json!(refreshed.expires));
                            object.insert(
                                "authRecoveryGrant".into(),
                                Value::String(auth_grant_digest(&refreshed.refresh)),
                            );
                        }
                        Err(error) => {
                            signal
                                .throw_if_aborted()
                                .map_err(|reason| anyhow::anyhow!("{reason}"))?;
                            if !is_grant_rejected(&error) {
                                return Err(error);
                            }
                            object.insert(
                                "authRecoveryGrant".into(),
                                Value::String(auth_grant_digest(&refresh)),
                            );
                        }
                    }
                    if let Some(accounts) = credential
                        .extra
                        .get_mut("accounts")
                        .and_then(Value::as_array_mut)
                    {
                        for slot in accounts.iter_mut() {
                            if slot.get("name").and_then(Value::as_str) == Some(name.as_str()) {
                                *slot = next.clone();
                            }
                        }
                    }
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    Ok(())
}

/// The re-probe time passed (or the block predates re-probe times): the next request probes this account.
fn is_probe_due(slot: &AccountSlot, now: f64) -> bool {
    slot.blocked_until.is_none_or(|until| until <= now)
}

/// Recovers before anything probes a rejected token: an account whose auth block is due for its
/// re-probe (senpi#3264) is refreshed first, so the probe sends fresh material; when no account can
/// serve, every recoverable account is.
pub async fn recover_auth_blocked_slots(
    input: AuthBlockRecoveryInput<'_>,
) -> anyhow::Result<Vec<AccountSlot>> {
    let grants = if input
        .accounts
        .iter()
        .any(|slot| slot.block_reason.as_deref() == Some("auth_error"))
    {
        stored_grants(input.store, input.provider_id).await?
    } else {
        Default::default()
    };
    let recoverable: Vec<&AccountSlot> = input
        .accounts
        .iter()
        .filter(|slot| is_auth_block_recoverable(slot, grants.get(&slot.name).map(String::as_str)))
        .collect();
    let candidates: Vec<&AccountSlot> = if input.accounts.iter().any(|slot| (input.is_usable)(slot))
    {
        recoverable
            .into_iter()
            .filter(|slot| is_probe_due(slot, input.now))
            .collect()
    } else {
        recoverable
    };
    if candidates.is_empty() {
        return Ok(input.accounts.clone());
    }
    let names: Vec<String> = candidates.iter().map(|slot| slot.name.clone()).collect();
    let mut transient_failure: Option<anyhow::Error> = None;
    for name in names {
        if let Err(error) = recover_slot(&input, &name).await {
            input
                .signal
                .throw_if_aborted()
                .map_err(|reason| anyhow::anyhow!("{reason}"))?;
            transient_failure = Some(error);
        }
    }
    let accounts = (input.reload)().await?;
    if let Some(failure) = transient_failure
        && !accounts.iter().any(|slot| (input.is_usable)(slot))
    {
        return Err((input.transient_error)(failure));
    }
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{add_account, empty_credential, list_accounts};
    use maho_ai::auth::credential_store::InMemoryCredentialStore;
    use maho_ai::utils::abort::AbortController;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PROVIDER: &str = "anthropic-subscription";

    fn blocked(name: &str, refresh: &str, until: Option<f64>) -> AccountSlot {
        AccountSlot {
            name: name.into(),
            display_name: None,
            refresh: refresh.into(),
            access: format!("{name}-access"),
            expires: 0.0,
            source: AccountSource::Login,
            blocked_until: until,
            block_reason: Some("auth_error".into()),
            ..Default::default()
        }
    }

    async fn seed(store: &InMemoryCredentialStore, slots: Vec<AccountSlot>) {
        let mut credential = empty_credential();
        for slot in slots {
            credential = add_account(&credential, slot).unwrap();
        }
        store
            .modify(
                PROVIDER,
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .unwrap();
    }

    async fn stored(store: &InMemoryCredentialStore) -> OAuthCredential {
        store
            .read(PROVIDER, None)
            .await
            .unwrap()
            .unwrap()
            .into_oauth()
            .unwrap()
    }

    fn input<'a>(
        store: &'a InMemoryCredentialStore,
        accounts: Vec<AccountSlot>,
        refresher: SlotRefresher,
        now: f64,
    ) -> AuthBlockRecoveryInput<'a> {
        AuthBlockRecoveryInput {
            store,
            provider_id: PROVIDER,
            accounts,
            refresher,
            signal: AbortController::new().signal(),
            is_grant_rejected: Arc::new(|error: &anyhow::Error| {
                error.to_string().contains("invalid_grant")
            }),
            reload: Box::new(move || -> BorrowedFuture<'a, Vec<AccountSlot>> {
                Box::pin(async move {
                    let credential = store
                        .read(PROVIDER, None)
                        .await?
                        .unwrap()
                        .into_oauth()
                        .unwrap();
                    list_accounts(&credential, None)
                })
            }),
            is_usable: Box::new(|slot: &AccountSlot| slot.block_reason.is_none()),
            now,
            transient_error: Box::new(|error: anyhow::Error| anyhow::anyhow!("transient: {error}")),
        }
    }

    #[test]
    fn recoverable_only_for_auth_blocks_with_unredeemed_material() {
        let slot = blocked("a", "r1", None);
        assert!(is_auth_block_recoverable(&slot, None));
        assert!(!is_auth_block_recoverable(
            &slot,
            Some(&auth_grant_digest("r1"))
        ));
        let mut env = slot.clone();
        env.source = AccountSource::Env;
        assert!(!is_auth_block_recoverable(&env, None));
        let mut rate = slot.clone();
        rate.block_reason = Some("rate_limit".into());
        assert!(!is_auth_block_recoverable(&rate, None));
        let mut empty = slot;
        empty.refresh.clear();
        assert!(!is_auth_block_recoverable(&empty, None));
        assert_eq!(auth_grant_digest("r1").len(), 32);
    }

    #[tokio::test]
    async fn recovers_when_no_account_is_usable_and_never_redeems_twice() {
        let store = InMemoryCredentialStore::new();
        let accounts = vec![blocked("a", "r1", Some(f64::MAX))];
        seed(&store, accounts.clone()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let refresher: SlotRefresher = Arc::new(
            move |refresh: String, _: AbortSignal| -> RecoveryFuture<OAuthCredential> {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    Ok(OAuthCredential::new(
                        "fresh",
                        format!("{refresh}-next"),
                        5.0,
                    ))
                })
            },
        );
        let result = recover_auth_blocked_slots(input(&store, accounts, refresher.clone(), 0.0))
            .await
            .unwrap();
        assert_eq!(result[0].block_reason, None);
        assert_eq!(result[0].access, "fresh");
        let credential = stored(&store).await;
        assert_eq!(
            credential.extra["accounts"][0]["authRecoveryGrant"],
            auth_grant_digest("r1-next")
        );
        // the recovered grant is never redeemed again by a later auth block
        let mut again = result[0].clone();
        again.block_reason = Some("auth_error".into());
        assert!(!is_stored_slot_recoverable(
            &credential.extra["accounts"][0]
        ));
        recover_auth_blocked_slots(input(&store, vec![again], refresher, 0.0))
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejected_grant_is_marked_and_slot_stays_blocked() {
        let store = InMemoryCredentialStore::new();
        let accounts = vec![blocked("a", "r1", None)];
        seed(&store, accounts.clone()).await;
        let refresher: SlotRefresher = Arc::new(
            |_: String, _: AbortSignal| -> RecoveryFuture<OAuthCredential> {
                Box::pin(async { Err(anyhow::anyhow!("invalid_grant")) })
            },
        );
        let result = recover_auth_blocked_slots(input(&store, accounts, refresher, 0.0))
            .await
            .unwrap();
        assert_eq!(result[0].block_reason.as_deref(), Some("auth_error"));
        let credential = stored(&store).await;
        assert_eq!(
            credential.extra["accounts"][0]["authRecoveryGrant"],
            auth_grant_digest("r1")
        );
    }

    #[tokio::test]
    async fn transient_failure_surfaces_only_when_nothing_is_usable() {
        let store = InMemoryCredentialStore::new();
        let accounts = vec![blocked("a", "r1", None)];
        seed(&store, accounts.clone()).await;
        let refresher: SlotRefresher = Arc::new(
            |_: String, _: AbortSignal| -> RecoveryFuture<OAuthCredential> {
                Box::pin(async { Err(anyhow::anyhow!("status=503")) })
            },
        );
        let error = recover_auth_blocked_slots(input(&store, accounts, refresher, 0.0))
            .await
            .unwrap_err();
        assert!(error.to_string().starts_with("transient:"));
        assert!(
            stored(&store).await.extra["accounts"][0]
                .get("authRecoveryGrant")
                .is_none()
        );
    }

    #[tokio::test]
    async fn with_a_usable_account_only_due_reprobes_recover() {
        let store = InMemoryCredentialStore::new();
        let mut usable = blocked("ok", "r0", None);
        usable.block_reason = None;
        let accounts = vec![
            usable,
            blocked("due", "r1", Some(10.0)),
            blocked("later", "r2", Some(1_000.0)),
        ];
        seed(&store, accounts.clone()).await;
        let refresher: SlotRefresher = Arc::new(
            |refresh: String, _: AbortSignal| -> RecoveryFuture<OAuthCredential> {
                Box::pin(async move {
                    Ok(OAuthCredential::new(
                        "fresh",
                        format!("{refresh}-next"),
                        5.0,
                    ))
                })
            },
        );
        let result = recover_auth_blocked_slots(input(&store, accounts, refresher, 100.0))
            .await
            .unwrap();
        let by_name = |name: &str| {
            result
                .iter()
                .find(|slot| slot.name == name)
                .unwrap()
                .clone()
        };
        assert_eq!(by_name("due").block_reason, None);
        assert_eq!(by_name("later").block_reason.as_deref(), Some("auth_error"));
    }
}
