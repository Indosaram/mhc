pub use crate::provider_id::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID;
use crate::{
    account_events::{AccountsChangedRemedy, emit_accounts_changed},
    accounts::{AccountSource, empty_credential, list_accounts, pin_account, remove_account},
    claude_cli_account_dir::{
        RemoveAccountDirOptions, RemoveAccountDirOutcome, account_dir_for, remove_account_dir,
    },
    oauth_login::{AnthropicSubscriptionOAuth, ClaudeCliLoginContext},
};
use maho_ai::auth::types::{
    Credential, CredentialStore, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use maho_ai::utils::abort::AbortSignal;
use std::{collections::BTreeMap, path::Path, sync::Arc};
#[derive(Debug, PartialEq)]
pub struct ProviderAccountSummary {
    pub name: String,
    pub display_name: Option<String>,
    pub source: AccountSource,
    pub blocked: bool,
    /// Same meaning as the generic account listing's fields (senpi#4327); present only while blocked.
    pub blocked_reason: Option<&'static str>,
    pub blocked_until: Option<f64>,
    pub pinned: bool,
}
/// senpi `timedBlock`: like the generic listing, a disabled account never lifts on its own and has no end time.
pub(crate) fn timed_block(
    blocked_until: Option<f64>,
    block_reason: Option<&str>,
    now: f64,
) -> (bool, Option<&'static str>, Option<f64>) {
    if block_reason == Some("account_disabled") {
        return (true, Some("disabled"), None);
    }
    let Some(until) = blocked_until.filter(|until| *until > now) else {
        return (false, None, None);
    };
    let reason = match block_reason {
        Some("auth_error") => Some("auth_error"),
        Some("rate_limit") => Some("rate_limit"),
        _ => None,
    };
    (true, reason, Some(until))
}
fn assert_managed_provider(provider: &str) -> anyhow::Result<()> {
    if provider != ANTHROPIC_SUBSCRIPTION_PROVIDER_ID {
        anyhow::bail!("Provider account management is unavailable for: {provider}");
    }
    Ok(())
}
fn credential_from(credential: Option<Credential>) -> anyhow::Result<OAuthCredential> {
    match credential {
        None => Ok(empty_credential()),
        Some(Credential::OAuth(credential)) => Ok(credential),
        Some(Credential::ApiKey(_)) => {
            anyhow::bail!("Provider account management requires an OAuth credential")
        }
    }
}

pub(crate) fn require_provider_consent(agent_dir: &Path) -> anyhow::Result<()> {
    let provider = ANTHROPIC_SUBSCRIPTION_PROVIDER_ID;
    if !agent_dir.is_absolute()
        || !agent_dir.to_str().is_some_and(|agent_dir| {
            maho_core::provider_consent::consent_recorded_in_agent_dir(agent_dir, provider)
        })
    {
        anyhow::bail!(
            "{}",
            maho_core::provider_consent::consent_required_message(provider)
        );
    }
    Ok(())
}

/// Adds an account through the provider's self-persisting login, only after SD-4 consent.
///
/// # Errors
/// Refuses if consent is not recorded in the active agent directory, or login fails.
pub async fn add_provider_account(
    oauth: &dyn OAuthAuth,
    interaction: &ProviderAuthInteraction,
    agent_dir: &Path,
) -> anyhow::Result<OAuthCredential> {
    require_provider_consent(agent_dir)?;
    let credential = oauth.login(interaction).await?;
    emit_accounts_changed(
        ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
        Some(AccountsChangedRemedy::Login),
    );
    Ok(credential)
}

pub async fn get_provider_accounts(
    store: &dyn CredentialStore,
    provider: &str,
    environment: &BTreeMap<String, String>,
    now: f64,
) -> anyhow::Result<Vec<ProviderAccountSummary>> {
    assert_managed_provider(provider)?;
    let credential = credential_from(store.read(provider, None).await?)?;
    Ok(
        list_accounts(&credential, Some(&|key| environment.get(key).cloned()))?
            .into_iter()
            .map(|account| {
                let display_name = maho_ai::auth::pool::slots::account_display_name(
                    account.display_name.as_deref(),
                );
                let (blocked, blocked_reason, blocked_until) =
                    timed_block(account.blocked_until, account.block_reason.as_deref(), now);
                ProviderAccountSummary {
                    blocked,
                    blocked_reason,
                    blocked_until,
                    pinned: credential.get_extra_str("pinned") == Some(&account.name),
                    name: account.name,
                    display_name,
                    source: account.source,
                }
            })
            .collect(),
    )
}
pub async fn unblock_provider_account(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
    environment: Arc<BTreeMap<String, String>>,
) -> anyhow::Result<bool> {
    unblock_provider_account_in_runtime(store, provider, name, environment, None).await
}
pub(crate) async fn unblock_provider_account_in_runtime(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
    environment: Arc<BTreeMap<String, String>>,
    runtime: Option<maho_ext_api::ExtensionRuntime>,
) -> anyhow::Result<bool> {
    assert_managed_provider(provider)?;
    let name = name.to_owned();
    let session_account = name.clone();
    let mutation_runtime = runtime.clone();
    let lifted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mutation_lifted = lifted.clone();
    store
        .modify(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    if let Some(runtime) = mutation_runtime {
                        runtime.assert_active()?;
                    }
                    let mut credential = credential_from(current.clone())?;
                    let account =
                        list_accounts(&credential, Some(&|key| environment.get(key).cloned()))?
                            .into_iter()
                            .find(|account| account.name == name)
                            .ok_or_else(|| anyhow::anyhow!("Provider account not found: {name}"))?;
                    if account.blocked_until.is_none()
                        || matches!(
                            account.block_reason.as_deref(),
                            Some("auth_error" | "account_disabled")
                        )
                    {
                        return Ok(current);
                    }
                    mutation_lifted.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(states) = credential
                        .extra
                        .get_mut("slotState")
                        .and_then(serde_json::Value::as_object_mut)
                        && let Some(state) = states
                            .get_mut(&name)
                            .and_then(serde_json::Value::as_object_mut)
                    {
                        state.remove("blockedUntil");
                        state.remove("blockReason");
                        state.remove("blockedAt");
                    }
                    if let Some(accounts) = credential
                        .extra
                        .get_mut("accounts")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for slot in accounts {
                            if slot["name"] == name
                                && let Some(slot) = slot.as_object_mut()
                            {
                                slot.remove("blockedUntil");
                                slot.remove("blockReason");
                                slot.remove("blockedAt");
                            }
                        }
                    }
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    if let Some(runtime) = runtime {
        runtime.assert_active()?;
    }
    let session_lifted = crate::failover::forget_session_blocks_for(&session_account);
    let lifted = lifted.load(std::sync::atomic::Ordering::SeqCst) || session_lifted;
    if lifted {
        emit_accounts_changed(provider, Some(AccountsChangedRemedy::Reset));
    }
    Ok(lifted)
}
pub async fn pin_provider_account(
    store: &dyn CredentialStore,
    provider: &str,
    name: Option<&str>,
    environment: Arc<BTreeMap<String, String>>,
) -> anyhow::Result<()> {
    pin_provider_account_in_runtime(store, provider, name, environment, None).await
}
pub(crate) async fn pin_provider_account_in_runtime(
    store: &dyn CredentialStore,
    provider: &str,
    name: Option<&str>,
    environment: Arc<BTreeMap<String, String>>,
    runtime: Option<maho_ext_api::ExtensionRuntime>,
) -> anyhow::Result<()> {
    assert_managed_provider(provider)?;
    let name = name.map(str::to_owned);
    let mutation_runtime = runtime.clone();
    store
        .modify(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    if let Some(runtime) = mutation_runtime {
                        runtime.assert_active()?;
                    }
                    let mut credential = credential_from(current.clone())?;
                    if let Some(name) = name {
                        if !list_accounts(&credential, Some(&|key| environment.get(key).cloned()))?
                            .iter()
                            .any(|account| account.name == name)
                        {
                            anyhow::bail!("Provider account not found: {name}");
                        }
                        credential = pin_account(&credential, &name);
                    } else if credential.extra.remove("pinned").is_none() {
                        return Ok(current);
                    }
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    if let Some(runtime) = runtime {
        runtime.assert_active()?;
    }
    crate::account_events::emit_provider_accounts_changed(provider);
    Ok(())
}
pub async fn remove_provider_account(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
    environment: Arc<BTreeMap<String, String>>,
) -> anyhow::Result<()> {
    remove_stored_account(store, provider, name, environment, None, None, None).await?;
    Ok(())
}

/// Result of dropping a slot. A notice means local removal succeeded but sign-out was incomplete.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RemoveProviderAccountResult {
    pub notice: Option<String>,
}

/// Drops an account and cleans up its CLI sign-in when it has a CLI account directory.
///
/// # Errors
/// Refuses environment accounts, missing accounts and failures removing a valid directory.
pub async fn remove_provider_account_with_cli(
    oauth: &AnthropicSubscriptionOAuth,
    name: &str,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<RemoveProviderAccountResult> {
    let environment = Arc::new(oauth.cli.environment.clone());
    remove_provider_account_in_runtime(oauth, name, environment, None, signal.cloned()).await
}

pub(crate) async fn remove_provider_account_in_runtime(
    oauth: &AnthropicSubscriptionOAuth,
    name: &str,
    environment: Arc<BTreeMap<String, String>>,
    runtime: Option<maho_ext_api::ExtensionRuntime>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<RemoveProviderAccountResult> {
    remove_stored_account(
        oauth.store.as_ref(),
        ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
        name,
        environment,
        Some(oauth.cli.clone()),
        runtime,
        signal,
    )
    .await
}

async fn remove_stored_account(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
    environment: Arc<BTreeMap<String, String>>,
    cli: Option<ClaudeCliLoginContext>,
    runtime: Option<maho_ext_api::ExtensionRuntime>,
    signal: Option<AbortSignal>,
) -> anyhow::Result<RemoveProviderAccountResult> {
    assert_managed_provider(provider)?;
    if let Some(runtime) = &runtime {
        runtime.assert_active()?;
    }
    if let Some(signal) = &signal {
        signal.throw_if_aborted()?;
    }
    let credential = credential_from(store.read(provider, None).await?)?;
    let account = list_accounts(&credential, Some(&|key| environment.get(key).cloned()))?
        .into_iter()
        .find(|account| account.name == name)
        .ok_or_else(|| anyhow::anyhow!("Provider account not found: {name}"))?;
    if account.source == AccountSource::Env {
        anyhow::bail!("Environment provider account cannot be removed: {name}");
    }
    let is_cli = account.is_claude_cli();
    let (result, sign_out_error) = if is_cli {
        let cli = cli.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Claude account removal requires the Claude CLI account context")
        })?;
        sign_out_cli_account(cli, name, account.dir_id.as_deref(), signal.as_ref()).await?
    } else {
        (RemoveProviderAccountResult::default(), None)
    };
    let slot_name = name.to_owned();
    let mutation_runtime = runtime.clone();
    let state_path = cli.as_ref().map(|cli| {
        cli.agent_dir
            .join(maho_core::credential_pool::state_store::CREDENTIAL_POOL_STATE_FILENAME)
    });
    store
        .modify_or_delete(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    let name = slot_name;
                    if !is_cli && let Some(runtime) = mutation_runtime {
                        runtime.assert_active()?;
                    }
                    let credential = credential_from(current)?;
                    let current =
                        list_accounts(&credential, Some(&|key| environment.get(key).cloned()))?
                            .into_iter()
                            .find(|account| account.name == name)
                            .ok_or_else(|| anyhow::anyhow!("Provider account not found: {name}"))?;
                    let same_identity = current.source == account.source
                        && current.store == account.store
                        && if is_cli {
                            current.dir_id == account.dir_id
                        } else {
                            current.access == account.access && current.refresh == account.refresh
                        };
                    if !same_identity {
                        anyhow::bail!("Provider account changed during removal: {name}");
                    }
                    if !is_cli && let Some(signal) = &signal {
                        signal.throw_if_aborted()?;
                    }
                    let mut next = remove_account(&credential, &name)?;
                    if let Some(states) = next
                        .extra
                        .get_mut("slotState")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        states.remove(&name);
                    }
                    if is_cli
                        && next
                            .extra
                            .get("accounts")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(Vec::is_empty)
                    {
                        Ok(None)
                    } else {
                        // Legacy callers have relied on the empty managed sentinel surviving.
                        Ok(Some(Credential::OAuth(next)))
                    }
                })
            }),
            None,
        )
        .await?;
    if let Some(state_path) = state_path {
        use maho_core::credential_pool::state_store::CredentialSlotRepository;
        let result = CredentialSlotRepository::new(&state_path.to_string_lossy())
            .mutate_slot_state(provider, "stored", name, |_| None)
            .await;
        if let Err(error) = result {
            maho_ext_api::diagnostic(&format!(
                "Failed to clear removed account's pool state: {error}"
            ));
        }
    }
    crate::account_events::emit_provider_accounts_changed(provider);
    if let Some(error) = sign_out_error {
        return Err(error);
    }
    if let Some(runtime) = runtime {
        runtime.assert_active()?;
    }
    Ok(result)
}

async fn sign_out_cli_account(
    cli: &ClaudeCliLoginContext,
    name: &str,
    dir_id: Option<&str>,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<(RemoveProviderAccountResult, Option<anyhow::Error>)> {
    let Some(dir_id) =
        dir_id.filter(|id| crate::claude_cli_account_dir::is_valid_account_dir_id(id))
    else {
        return Ok((
            RemoveProviderAccountResult {
                notice: Some(format!(
                    "Removed Claude account {name} without a Claude sign-out: it names no valid account dir."
                )),
            },
            None,
        ));
    };
    let account_dir = account_dir_for(&cli.agent_dir, dir_id)?;
    let existed = account_dir.symlink_metadata().is_ok();
    let executable = (cli.resolve)().ok().map(|(path, _)| path);
    let outcome = remove_account_dir(RemoveAccountDirOptions {
        agent_dir: &cli.agent_dir,
        dir_id,
        executable: executable.as_deref(),
        parent_env: &cli.environment,
        signal,
    })
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error)
            if existed
                && matches!(
                    account_dir.symlink_metadata(),
                    Err(missing) if missing.kind() == std::io::ErrorKind::NotFound
                ) =>
        {
            // The CLI removed its directory before failing. Drop the orphaned slot, then report the failure.
            return Ok((RemoveProviderAccountResult::default(), Some(error)));
        }
        Err(error) => return Err(error),
    };
    let notice = match outcome {
        RemoveAccountDirOutcome::Removed => None,
        RemoveAccountDirOutcome::RemovedWithoutSignOut => Some(format!(
            "Removed Claude account {name}, but the Claude CLI sign-out could not run; its stored sign-in may remain in the Claude CLI's credential store."
        )),
        RemoveAccountDirOutcome::NotAnAccountDir => Some(format!(
            "Removed Claude account {name} without a Claude sign-out: its account dir is a link or a file, left untouched at {}.",
            account_dir.display()
        )),
    };
    Ok((RemoveProviderAccountResult { notice }, None))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountSlot, add_account};
    use maho_ai::auth::credential_store::InMemoryCredentialStore;
    use maho_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt};

    #[tokio::test]
    async fn add_refuses_before_login_until_base_consent_is_recorded() {
        struct Fixture;
        #[async_trait::async_trait]
        impl AuthInteraction for Fixture {
            fn signal(&self) -> Option<AbortSignal> {
                None
            }
            async fn prompt(&self, _: AuthPrompt) -> anyhow::Result<String> {
                anyhow::bail!("unexpected prompt");
            }
            fn notify(&self, _: AuthEvent) {}
        }
        #[async_trait::async_trait]
        impl OAuthAuth for Fixture {
            fn name(&self) -> &str {
                "fixture"
            }
            async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
                anyhow::bail!("fixture login was called");
            }
            async fn refresh(
                &self,
                _: &OAuthCredential,
                _: &AbortSignal,
            ) -> anyhow::Result<OAuthCredential> {
                anyhow::bail!("unexpected refresh");
            }
            async fn to_auth(
                &self,
                _: &OAuthCredential,
            ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
                anyhow::bail!("unexpected auth conversion");
            }
        }

        let agent = tempfile::tempdir().expect("isolated agent dir");
        let settings = agent.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"providerRiskConsent":{"anthropic-subscription:rotation":"fixture"}}"#,
        )
        .expect("rotation-only settings");
        let controller = maho_ai::utils::abort::AbortController::new();
        let interaction = ProviderAuthInteraction::new(controller.signal(), Arc::new(Fixture));
        let refused = add_provider_account(&Fixture, &interaction, agent.path())
            .await
            .expect_err("no base consent");
        assert!(
            refused
                .to_string()
                .contains("mhc auth consent anthropic-subscription")
        );

        std::fs::write(
            &settings,
            r#"{"providerRiskConsent":{"anthropic-subscription":"fixture"}}"#,
        )
        .expect("base consent settings");
        let reached = add_provider_account(&Fixture, &interaction, agent.path())
            .await
            .expect_err("fixture login result");
        assert_eq!(reached.to_string(), "fixture login was called");
    }

    #[tokio::test]
    async fn resetting_usage_preserves_model_blocks_and_auth_rejections() {
        let store = InMemoryCredentialStore::new();
        let provider = ANTHROPIC_SUBSCRIPTION_PROVIDER_ID;
        let credential = empty_credential().with_extra(
            "accounts",
            serde_json::json!([
                {"name":"work","blockReason":"rate_limit","blockedUntil":100,"blockedAt":10,
                 "modelBlocks":{"fable":{"blockedUntil":150}}},
                {"name":"auth","blockReason":"auth_error","blockedUntil":100,"blockedAt":20}
            ]),
        );
        store
            .modify(
                provider,
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("seed");

        assert!(
            unblock_provider_account(&store, provider, "work", Arc::default())
                .await
                .expect("usage reset")
        );
        assert!(
            !unblock_provider_account(&store, provider, "auth", Arc::default())
                .await
                .expect("auth retained")
        );
        let saved = store
            .read(provider, None)
            .await
            .expect("read")
            .and_then(Credential::into_oauth)
            .expect("credential");
        let slots = list_accounts(&saved, None).expect("slots");
        assert_eq!(
            (
                slots[0].blocked_until,
                slots[0].blocked_at,
                slots[0].block_reason.as_deref()
            ),
            (None, None, None)
        );
        assert_eq!(
            slots[0].model_blocks.as_ref().expect("model block")["fable"].blocked_until,
            150.0
        );
        assert_eq!(slots[1].block_reason.as_deref(), Some("auth_error"));
    }

    #[tokio::test]
    async fn legacy_removal_refuses_to_orphan_a_cli_slot_without_cleanup_context() {
        let store = InMemoryCredentialStore::new();
        let provider = ANTHROPIC_SUBSCRIPTION_PROVIDER_ID;
        let credential = empty_credential().with_extra(
            "accounts",
            serde_json::json!([
                {"name":"work","store":"claude-cli","dirId":"0123456789abcdef0123456789abcdef"}
            ]),
        );
        let before = Credential::OAuth(credential);
        let seeded = before.clone();
        store
            .modify(
                provider,
                Box::new(move |_| Box::pin(async move { Ok(Some(seeded)) })),
                None,
            )
            .await
            .expect("seed");

        let error = remove_provider_account(&store, provider, "work", Arc::default())
            .await
            .expect_err("no cleanup context");
        assert_eq!(
            error.to_string(),
            "Claude account removal requires the Claude CLI account context"
        );
        assert_eq!(
            store.read(provider, None).await.expect("read"),
            Some(before)
        );
    }

    async fn seeded() -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        let credential = add_account(
            &empty_credential(),
            AccountSlot {
                name: "primary".into(),
                display_name: Some("  Main  ".into()),
                access: String::new(),
                refresh: String::new(),
                expires: 0.0,
                source: AccountSource::Login,
                blocked_until: Some(100.0),
                block_reason: None,
                ..Default::default()
            },
        )
        .expect("slot");
        store
            .modify(
                ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("seed");
        store
    }
    #[tokio::test]
    async fn summaries_include_environment_pin_and_live_block_state() {
        let store = seeded().await;
        let environment: Arc<BTreeMap<String, String>> = Arc::new(
            [(
                String::from("CLAUDE_CODE_OAUTH_TOKEN"),
                String::from("fixture"),
            )]
            .into(),
        );
        pin_provider_account(
            &store,
            ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            Some("env"),
            environment.clone(),
        )
        .await
        .expect("pin");
        let summaries = get_provider_accounts(
            &store,
            ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            &environment,
            50.0,
        )
        .await
        .expect("summaries");
        assert_eq!(summaries.len(), 2);
        assert!(summaries[0].blocked);
        assert_eq!(summaries[0].display_name.as_deref(), Some("Main"));
        assert!(summaries[1].pinned);
        pin_provider_account(
            &store,
            ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            None,
            environment.clone(),
        )
        .await
        .expect("unpin");
        assert!(
            !get_provider_accounts(
                &store,
                ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
                &environment,
                101.0
            )
            .await
            .expect("summaries")[0]
                .blocked
        );
    }
    #[test]
    fn blocked_summaries_carry_reason_and_end_time() {
        assert_eq!(
            timed_block(Some(100.0), Some("rate_limit"), 50.0),
            (true, Some("rate_limit"), Some(100.0))
        );
        assert_eq!(
            timed_block(Some(100.0), Some("auth_error"), 50.0),
            (true, Some("auth_error"), Some(100.0))
        );
        assert_eq!(
            timed_block(Some(100.0), Some("other"), 50.0),
            (true, None, Some(100.0))
        );
        assert_eq!(
            timed_block(Some(100.0), Some("rate_limit"), 150.0),
            (false, None, None)
        );
        assert_eq!(
            timed_block(None, Some("account_disabled"), 150.0),
            (true, Some("disabled"), None)
        );
        assert_eq!(
            timed_block(Some(10.0), Some("account_disabled"), 150.0),
            (true, Some("disabled"), None)
        );
        assert_eq!(timed_block(None, None, 0.0), (false, None, None));
    }
    #[tokio::test]
    async fn removal_refuses_environment_and_missing_slots_and_unpins_stored_slot() {
        let store = seeded().await;
        let environment: Arc<BTreeMap<String, String>> = Arc::new(
            [(
                String::from("CLAUDE_CODE_OAUTH_TOKEN"),
                String::from("fixture"),
            )]
            .into(),
        );
        for name in ["env", "missing"] {
            assert!(
                remove_provider_account(
                    &store,
                    ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
                    name,
                    environment.clone()
                )
                .await
                .is_err()
            );
        }
        pin_provider_account(
            &store,
            ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            Some("primary"),
            environment.clone(),
        )
        .await
        .expect("pin");
        remove_provider_account(
            &store,
            ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            "primary",
            environment.clone(),
        )
        .await
        .expect("remove");
        let summaries = get_provider_accounts(
            &store,
            ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            &environment,
            0.0,
        )
        .await
        .expect("summaries");
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].pinned);
    }
    #[tokio::test]
    async fn unsupported_provider_and_api_key_credentials_are_rejected() {
        let store = InMemoryCredentialStore::new();
        assert!(
            get_provider_accounts(&store, "other", &BTreeMap::new(), 0.0)
                .await
                .is_err()
        );
        store
            .modify(
                ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
                Box::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::ApiKey(
                            maho_ai::auth::types::ApiKeyCredential {
                                key: Some("fixture".into()),
                                env: None,
                                ..Default::default()
                            },
                        )))
                    })
                }),
                None,
            )
            .await
            .expect("seed");
        assert!(
            get_provider_accounts(
                &store,
                ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
                &BTreeMap::new(),
                0.0
            )
            .await
            .is_err()
        );
    }
}
