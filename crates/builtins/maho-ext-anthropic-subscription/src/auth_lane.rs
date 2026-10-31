use crate::{
    accounts::{AccountSlot, AccountSource, env_slot_token},
    auth_environment::{
        has_request_oauth_token, merge_request_auth_environment, strip_managed_auth_environment,
    },
    settings::ProviderSettings,
};
use sha2::Digest;
use std::{collections::BTreeMap, path::Path, sync::Arc};
pub const EXPIRING_WITHIN_MS: f64 = 5.0 * 60000.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenInjection {
    Ambient,
    OAuthSlots,
    ConfigDir,
}
pub const PROVIDER_ID: &str = "anthropic-subscription";
pub struct ManagedPool {
    pub accounts: Vec<AccountSlot>,
    pub environment: BTreeMap<String, String>,
    pub lane: TokenInjection,
    pub pinned_account: Option<String>,
    pub pin_source: Option<crate::affinity::PinSource>,
    /// The user's stored account order (senpi#3591).
    pub account_order: Option<Vec<String>>,
}
/// The spawn-time projection of a selected slot: the child environment and the digest of the access token it
/// authenticates with, so a resident process spawned with another token never serves the turn.
pub struct PreparedSlot {
    pub env: BTreeMap<String, String>,
    pub credential_digest: String,
}
const TRANSIENT_REFRESH_FAILURE: &str = r"\bstatus=5\d\d\b|\bTimeoutError\b";
/// senpi `refreshFailure`: throttling, server errors and timeouts on the token endpoint are not a verdict on the
/// grant; anything else is an authentication failure.
pub fn refresh_failure(error: &anyhow::Error) -> anyhow::Error {
    static TRANSIENT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(TRANSIENT_REFRESH_FAILURE).expect("transient refresh pattern")
    });
    let detail = error.to_string();
    let classification = crate::errors::classify_sdk_error(&serde_json::json!(detail));
    if matches!(
        classification.kind,
        crate::errors::SdkErrorKind::RateLimit | crate::errors::SdkErrorKind::Overloaded
    ) {
        return anyhow::anyhow!("{detail}");
    }
    if (classification.kind == crate::errors::SdkErrorKind::Other && classification.retryable)
        || TRANSIENT.is_match(&detail)
    {
        return anyhow::anyhow!("server_error: {detail}");
    }
    anyhow::anyhow!("authentication_failed: {detail}")
}
/// senpi `isGrantRejected`: the token endpoint rejected the grant itself, not throttled or unavailable.
pub fn is_grant_rejected(error: &anyhow::Error) -> bool {
    refresh_failure(error)
        .to_string()
        .starts_with("authentication_failed:")
}
/// Whether a store write failed only because another writer held the credential lock past its budget.
pub fn is_store_busy(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<maho_core::lockfile_policy::CredentialStoreBusyError>()
        .is_some()
        || error.to_string().starts_with("Credential store is busy")
}
impl ManagedPool {
    fn stored_accounts(
        &self,
        credential: Option<&maho_ai::auth::types::Credential>,
    ) -> anyhow::Result<Vec<AccountSlot>> {
        let empty = crate::accounts::empty_credential();
        crate::accounts::list_accounts(
            credential
                .and_then(|value| value.as_oauth())
                .unwrap_or(&empty),
            Some(&|name| self.environment.get(name).cloned()),
        )
    }
    /// Re-reads the stored slots, so auth-block recovery sees what it wrote.
    pub async fn reload_accounts(
        &self,
        store: &dyn maho_ai::auth::types::CredentialStore,
    ) -> anyhow::Result<Vec<AccountSlot>> {
        self.stored_accounts(store.read(PROVIDER_ID, None).await?.as_ref())
    }
    /// Another writer holds auth.json past the lock budget, almost always a sibling session redeeming this slot.
    /// Contention says nothing about the credential: adopt the sibling's rotated token, or keep the stored one while
    /// it is still inside its lifetime.
    async fn continue_while_store_busy(
        &self,
        store: &dyn maho_ai::auth::types::CredentialStore,
        selected: &mut AccountSlot,
        now: f64,
        busy: anyhow::Error,
    ) -> anyhow::Result<()> {
        let latest = self
            .reload_accounts(store)
            .await?
            .into_iter()
            .find(|candidate| candidate.name == selected.name);
        if let Some(latest) = latest
            && latest.refresh != selected.refresh
        {
            *selected = latest;
            return Ok(());
        }
        if now < selected.expires {
            return Ok(());
        }
        Err(busy)
    }
    pub async fn refresh_selected<F, Fut>(
        &self,
        store: &dyn maho_ai::auth::types::CredentialStore,
        selected: &mut AccountSlot,
        now: f64,
        signal: maho_ai::utils::abort::AbortSignal,
        refresh: F,
    ) -> anyhow::Result<()>
    where
        F: FnOnce(String, maho_ai::utils::abort::AbortSignal) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<maho_ai::auth::types::OAuthCredential>>
            + Send
            + 'static,
    {
        if selected.source == AccountSource::Env || now < selected.expires - EXPIRING_WITHIN_MS {
            return Ok(());
        }
        let abort = signal.clone();
        let refreshed = crate::accounts::refresh_slot(
            store,
            PROVIDER_ID,
            &selected.name,
            refresh,
            signal,
            move |expires| now >= expires - EXPIRING_WITHIN_MS,
        )
        .await;
        let update = match refreshed {
            Ok(credential) => self
                .stored_accounts(credential.as_ref())?
                .into_iter()
                .find(|candidate| candidate.name == selected.name)
                .ok_or_else(|| anyhow::anyhow!("selected account disappeared during refresh")),
            Err(error) => Err(error),
        };
        match update {
            Ok(updated) => {
                *selected = updated;
                Ok(())
            }
            Err(error) => {
                // A cancelled turn is not an authentication verdict: an aborted refresh must not auth-block it.
                abort
                    .throw_if_aborted()
                    .map_err(|reason| anyhow::anyhow!("{reason}"))?;
                if !is_store_busy(&error) {
                    return Err(refresh_failure(&error));
                }
                self.continue_while_store_busy(store, selected, now, error)
                    .await
            }
        }
    }
    pub async fn refresh_for_attempt(
        &self,
        store: &dyn maho_ai::auth::types::CredentialStore,
        selected: &mut AccountSlot,
        now: f64,
        signal: maho_ai::utils::abort::AbortSignal,
        refresh: Arc<dyn maho_ai::auth::types::OAuthAuth>,
    ) -> anyhow::Result<()> {
        let access = selected.access.clone();
        let refresh_token = selected.refresh.clone();
        let expires = selected.expires;
        self.refresh_selected(store, selected, now, signal, move |_, signal| async move {
            refresh
                .refresh(
                    &maho_ai::auth::types::OAuthCredential::new(access, refresh_token, expires),
                    &signal,
                )
                .await
        })
        .await
    }
}
pub async fn managed_pool(
    store: &dyn maho_ai::auth::types::CredentialStore,
    settings: &ProviderSettings,
    host: &BTreeMap<String, String>,
    request: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<Option<ManagedPool>> {
    use maho_ai::auth::types::Credential;
    let mut credential = store.read(PROVIDER_ID, None).await?;
    let environment = merge_request_auth_environment(host, request);
    let read_env = |name: &str| environment.get(name).cloned();
    let empty = crate::accounts::empty_credential();
    let mut accounts = crate::accounts::list_accounts(
        credential
            .as_ref()
            .and_then(|value| value.as_oauth())
            .unwrap_or(&empty),
        Some(&read_env),
    )?;
    if credential.is_none() && !accounts.is_empty() {
        credential = store
            .modify(
                PROVIDER_ID,
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::OAuth(crate::accounts::empty_credential())))
                    })
                }),
                None,
            )
            .await?;
        accounts = crate::accounts::list_accounts(
            credential
                .as_ref()
                .and_then(|value| value.as_oauth())
                .unwrap_or(&empty),
            Some(&read_env),
        )?;
    }
    let lane = request_lane(settings, &accounts, request)?;
    if lane == TokenInjection::Ambient {
        return Ok(None);
    }
    let settings_pin = settings
        .values
        .get("pinnedAccount")
        .and_then(serde_json::Value::as_str);
    let stored_pin = credential
        .as_ref()
        .and_then(|value| value.as_oauth())
        .and_then(|value| value.get_extra_str("pinned"));
    let pin_source = if settings_pin.is_some() {
        Some(crate::affinity::PinSource::Settings)
    } else if stored_pin.is_some() {
        Some(crate::affinity::PinSource::Stored)
    } else {
        None
    };
    let pinned_account = settings_pin.or(stored_pin).map(str::to_owned);
    let account_order = credential
        .as_ref()
        .and_then(|value| value.as_oauth())
        .and_then(|value| value.get_extra("order"))
        .and_then(|order| serde_json::from_value(order.clone()).ok());
    Ok(Some(ManagedPool {
        accounts,
        environment,
        lane,
        pinned_account,
        pin_source,
        account_order,
    }))
}
pub fn resolve_effective_lane(
    settings: &ProviderSettings,
    accounts: &[AccountSlot],
) -> TokenInjection {
    match settings
        .values
        .get("tokenInjection")
        .and_then(serde_json::Value::as_str)
    {
        Some("ambient") => TokenInjection::Ambient,
        Some("oauth-slots") => TokenInjection::OAuthSlots,
        Some("config-dir") => TokenInjection::ConfigDir,
        _ => {
            if accounts.is_empty() {
                TokenInjection::Ambient
            } else {
                TokenInjection::OAuthSlots
            }
        }
    }
}
pub fn request_lane(
    settings: &ProviderSettings,
    accounts: &[AccountSlot],
    request: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<TokenInjection> {
    let mut lane = resolve_effective_lane(settings, accounts);
    if lane == TokenInjection::ConfigDir && has_request_oauth_token(request) {
        lane = TokenInjection::OAuthSlots;
    }
    if lane != TokenInjection::Ambient && accounts.is_empty() {
        anyhow::bail!(
            "authentication_failed: No Anthropic Subscription accounts configured for the managed lane; run /login anthropic-subscription or set CLAUDE_CODE_OAUTH_TOKEN"
        );
    }
    Ok(lane)
}
pub fn ambient_environment(
    host: &BTreeMap<String, String>,
    request: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut environment = merge_request_auth_environment(host, request);
    environment.retain(|name, _| !name.starts_with("SENPI_"));
    environment
}
/// The caller refreshes stored slots before invoking this spawn-time projection.
pub fn prepared_environment(
    environment: &BTreeMap<String, String>,
    lane: TokenInjection,
    slot: &AccountSlot,
    agent_dir: &Path,
    now_ms: u64,
) -> anyhow::Result<PreparedSlot> {
    let access = if slot.source == AccountSource::Env {
        env_slot_token(&|name| environment.get(name).cloned(), &slot.name)
    } else {
        Some(slot.access.clone())
    }
    .filter(|token| !token.is_empty())
    .ok_or_else(|| anyhow::anyhow!("authentication_failed: selected OAuth token is unavailable"))?;
    let mut child = strip_managed_auth_environment(environment);
    let credential_digest = format!("{:x}", sha2::Sha256::digest(access.as_bytes()));
    match lane {
        TokenInjection::OAuthSlots => {
            child.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), access);
        }
        TokenInjection::ConfigDir => {
            let directory = crate::config_dir_credentials::write_config_dir_credential(
                agent_dir, slot, &access, now_ms,
            )?;
            child.insert(
                "CLAUDE_CONFIG_DIR".into(),
                directory.to_string_lossy().into_owned(),
            );
        }
        TokenInjection::Ambient => anyhow::bail!("ambient lane does not prepare a managed slot"),
    };
    Ok(PreparedSlot {
        env: child,
        credential_digest,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn refresh_at_expiry_margin_updates_selected_slot_before_spawn() {
        use maho_ai::{
            auth::{
                credential_store::InMemoryCredentialStore,
                types::{Credential, CredentialStore, OAuthCredential},
            },
            utils::abort::AbortController,
        };
        let store = InMemoryCredentialStore::new();
        let mut selected = slot();
        selected.name = "work".into();
        selected.source = AccountSource::Login;
        selected.access = "expired".into();
        selected.refresh = "synthetic-refresh".into();
        selected.expires = EXPIRING_WITHIN_MS;
        let credential =
            crate::accounts::add_account(&crate::accounts::empty_credential(), selected.clone())
                .expect("account");
        store
            .modify(
                PROVIDER_ID,
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("seed");
        let pool = ManagedPool {
            accounts: vec![selected.clone()],
            environment: BTreeMap::new(),
            lane: TokenInjection::OAuthSlots,
            pinned_account: None,
            pin_source: None,
            account_order: None,
        };
        pool.refresh_selected(
            &store,
            &mut selected,
            0.0,
            AbortController::new().signal(),
            |_, _| async { Ok(OAuthCredential::new("fresh", "fresh-refresh", 1000000.0)) },
        )
        .await
        .expect("refresh");
        assert_eq!(selected.access, "fresh");
        let directory = tempfile::tempdir().expect("dir");
        assert_eq!(
            prepared_environment(&pool.environment, pool.lane, &selected, directory.path(), 0)
                .expect("spawn environment")
                .env["CLAUDE_CODE_OAUTH_TOKEN"],
            "fresh"
        );
    }
    #[tokio::test]
    async fn env_only_discovery_seeds_sentinel_and_settings_pin_wins() {
        use maho_ai::auth::{credential_store::InMemoryCredentialStore, types::CredentialStore};
        let store = InMemoryCredentialStore::new();
        let environment = [("CLAUDE_CODE_OAUTH_TOKEN".into(), "synthetic".into())].into();
        let settings = crate::settings::load(
            &serde_json::json!({"anthropicSubscriptionProvider":{"pinnedAccount":"env"}}),
            &serde_json::Value::Null,
            &BTreeMap::new(),
        );
        let pool = managed_pool(&store, &settings, &environment, None)
            .await
            .expect("pool")
            .expect("managed");
        assert_eq!(pool.lane, TokenInjection::OAuthSlots);
        assert_eq!(pool.accounts.len(), 1);
        assert_eq!(pool.pinned_account.as_deref(), Some("env"));
        assert_eq!(pool.pin_source, Some(crate::affinity::PinSource::Settings));
        let credential = store
            .read(PROVIDER_ID, None)
            .await
            .expect("read")
            .expect("credential")
            .into_oauth()
            .expect("oauth");
        crate::accounts::assert_sentinel_invariant(&credential).expect("sentinel");
        assert!(
            crate::accounts::list_accounts(&credential, None)
                .expect("stored accounts")
                .is_empty()
        );
    }
    fn settings(lane: &str) -> ProviderSettings {
        crate::settings::load(
            &serde_json::json!({"anthropicSubscriptionProvider":{"tokenInjection":lane}}),
            &serde_json::Value::Null,
            &BTreeMap::new(),
        )
    }
    fn slot() -> AccountSlot {
        AccountSlot {
            name: "env".into(),
            display_name: None,
            refresh: String::new(),
            access: String::new(),
            expires: 0.0,
            source: AccountSource::Env,
            blocked_until: None,
            block_reason: None,
            ..Default::default()
        }
    }
    #[test]
    fn managed_empty_pool_refuses_and_request_token_changes_config_root_lane() {
        assert!(request_lane(&settings("oauth-slots"), &[], None).is_err());
        assert_eq!(
            request_lane(&settings("ambient"), &[], None).expect("ambient"),
            TokenInjection::Ambient
        );
        let request = [("CLAUDE_CODE_OAUTH_TOKEN".into(), "synthetic".into())].into();
        assert_eq!(
            request_lane(&settings("config-dir"), &[slot()], Some(&request)).expect("request"),
            TokenInjection::OAuthSlots
        );
    }
    #[test]
    fn managed_spawn_uses_only_selected_token_and_ambient_keeps_host_auth() {
        let environment: BTreeMap<_, _> = [
            ("CLAUDE_CODE_OAUTH_TOKEN".into(), "synthetic".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN_2".into(), "other".into()),
            ("ANTHROPIC_API_KEY".into(), "host".into()),
            ("SENPI_PRIVATE".into(), "marker".into()),
            ("PATH".into(), "/bin".into()),
        ]
        .into();
        let directory = tempfile::tempdir().expect("dir");
        let child = prepared_environment(
            &environment,
            TokenInjection::OAuthSlots,
            &slot(),
            directory.path(),
            0,
        )
        .expect("prepare");
        assert_eq!(
            child.credential_digest,
            format!("{:x}", sha2::Sha256::digest(b"synthetic"))
        );
        let child = child.env;
        assert_eq!(child.len(), 2);
        assert_eq!(child["CLAUDE_CODE_OAUTH_TOKEN"], "synthetic");
        let ambient = ambient_environment(&environment, None);
        assert!(ambient.contains_key("ANTHROPIC_API_KEY"));
        assert!(!ambient.contains_key("SENPI_PRIVATE"));
    }

    #[test]
    fn refresh_failures_keep_throttling_and_server_errors_off_the_grant() {
        let map = |text: &str| refresh_failure(&anyhow::anyhow!("{text}")).to_string();
        assert_eq!(map("rate_limit_error"), "rate_limit_error");
        assert_eq!(
            map("token endpoint status=503"),
            "server_error: token endpoint status=503"
        );
        assert_eq!(map("invalid_grant"), "authentication_failed: invalid_grant");
        assert!(is_grant_rejected(&anyhow::anyhow!("invalid_grant")));
        assert!(!is_grant_rejected(&anyhow::anyhow!(
            "token endpoint status=502"
        )));
        assert!(is_store_busy(&anyhow::Error::new(
            maho_core::lockfile_policy::CredentialStoreBusyError::new("auth.json", 10)
        )));
    }
    use maho_ai::auth::types::CredentialStore as _;
    struct BusyStore(maho_ai::auth::credential_store::InMemoryCredentialStore);
    #[async_trait::async_trait]
    impl maho_ai::auth::types::CredentialStore for BusyStore {
        async fn read(
            &self,
            provider_id: &str,
            options: Option<maho_ai::auth::types::AuthOperationOptions>,
        ) -> anyhow::Result<Option<maho_ai::auth::types::Credential>> {
            self.0.read(provider_id, options).await
        }
        async fn list(
            &self,
            options: Option<maho_ai::auth::types::AuthOperationOptions>,
        ) -> anyhow::Result<Vec<maho_ai::auth::types::CredentialInfo>> {
            self.0.list(options).await
        }
        async fn modify(
            &self,
            _: &str,
            _: Box<
                dyn FnOnce(
                        Option<maho_ai::auth::types::Credential>,
                    ) -> std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = anyhow::Result<
                                        Option<maho_ai::auth::types::Credential>,
                                    >,
                                > + Send,
                        >,
                    > + Send,
            >,
            _: Option<maho_ai::auth::types::AuthOperationOptions>,
        ) -> anyhow::Result<Option<maho_ai::auth::types::Credential>> {
            Err(anyhow::Error::new(
                maho_core::lockfile_policy::CredentialStoreBusyError::new("auth.json", 10),
            ))
        }
        async fn modify_or_delete(
            &self,
            provider_id: &str,
            f: Box<
                dyn FnOnce(
                        Option<maho_ai::auth::types::Credential>,
                    ) -> std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = anyhow::Result<
                                        Option<maho_ai::auth::types::Credential>,
                                    >,
                                > + Send,
                        >,
                    > + Send,
            >,
            options: Option<maho_ai::auth::types::AuthOperationOptions>,
        ) -> anyhow::Result<Option<maho_ai::auth::types::Credential>> {
            self.modify(provider_id, f, options).await
        }
        async fn delete(
            &self,
            provider_id: &str,
            options: Option<maho_ai::auth::types::AuthOperationOptions>,
        ) -> anyhow::Result<()> {
            self.0.delete(provider_id, options).await
        }
    }
    #[tokio::test]
    async fn a_busy_store_keeps_a_live_token_and_adopts_a_sibling_rotation() {
        use maho_ai::{
            auth::{
                credential_store::InMemoryCredentialStore,
                types::{Credential, CredentialStore, OAuthCredential},
            },
            utils::abort::AbortController,
        };
        let mut selected = slot();
        selected.name = "work".into();
        selected.source = AccountSource::Login;
        selected.access = "current".into();
        selected.refresh = "current-refresh".into();
        selected.expires = 100.0;
        let pool = ManagedPool {
            accounts: vec![selected.clone()],
            environment: BTreeMap::new(),
            lane: TokenInjection::OAuthSlots,
            pinned_account: None,
            pin_source: None,
            account_order: None,
        };
        let refresh = |_: String, _| async { Ok(OAuthCredential::new("unused", "unused", 0.0)) };
        for (stored_refresh, now, outcome) in [
            ("current-refresh", 50.0, Some("current")),
            ("sibling-refresh", 500.0, Some("sibling")),
            ("current-refresh", 500.0, None),
        ] {
            let inner = InMemoryCredentialStore::new();
            let mut stored = selected.clone();
            stored.refresh = stored_refresh.into();
            stored.access = if stored_refresh == "current-refresh" {
                "current"
            } else {
                "sibling"
            }
            .into();
            let credential =
                crate::accounts::add_account(&crate::accounts::empty_credential(), stored)
                    .expect("account");
            inner
                .modify(
                    PROVIDER_ID,
                    Box::new(move |_| {
                        Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })
                    }),
                    None,
                )
                .await
                .expect("seed");
            let store = BusyStore(inner);
            let mut attempt = selected.clone();
            let result = pool
                .refresh_selected(
                    &store,
                    &mut attempt,
                    now,
                    AbortController::new().signal(),
                    refresh,
                )
                .await;
            match outcome {
                Some(access) => {
                    result.expect("continues while busy");
                    assert_eq!(attempt.access, access);
                }
                None => assert!(is_store_busy(
                    &result.expect_err("expired token stays busy")
                )),
            }
        }
    }
}
