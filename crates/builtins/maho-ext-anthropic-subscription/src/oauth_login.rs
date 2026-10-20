use crate::accounts::{
    AccountSlot, AccountSource, SENTINEL_TOKEN, add_account, empty_credential, list_accounts,
    upsert_account,
};
use maho_ai::{
    auth::types::{
        AuthCheck, AuthContext, AuthPrompt, AuthPromptKind, AuthResult, AuthType, Credential,
        CredentialStore, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
    },
    utils::abort::AbortSignal,
};
use std::{collections::BTreeMap, sync::Arc};

pub const PROVIDER_NAME: &str = "Anthropic Subscription (Claude Pro/Max)";
pub struct AnthropicSubscriptionOAuth {
    pub store: Arc<dyn CredentialStore>,
    pub flow: Arc<dyn OAuthAuth>,
    pub settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
    pub ambient: Arc<crate::availability::AmbientAuthStatusReader>,
}

impl AnthropicSubscriptionOAuth {
    pub fn native(
        store: Arc<dyn CredentialStore>,
        settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
        executable: std::path::PathBuf,
    ) -> Self {
        Self::native_with_resolver(store, settings, Arc::new(move || -> anyhow::Result<(std::path::PathBuf, Option<crate::executable::ExecutableSource>)> { Ok((executable.clone(), None)) }))
    }
    /// senpi `probeAmbientClaudeAuthStatus`: the executable is resolved per probe and an
    /// unresolvable binary reports the ambient lane unavailable instead of failing.
    pub fn native_with_resolver(
        store: Arc<dyn CredentialStore>,
        settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
        resolve: crate::executable::ExecutableResolver,
    ) -> Self {
        let ambient = crate::availability::AmbientAuthStatusReader::new(
            Arc::new(move || {
                let executable = resolve().map(|(executable, _)| executable);
                Box::pin(async move {
                    let Ok(executable) = executable else {
                        return Ok(false);
                    };
                    Ok(crate::availability::probe_ambient_claude_auth_status(
                        &executable,
                        crate::availability::AMBIENT_PROBE_TIMEOUT_MS,
                    )
                    .await)
                })
            }),
            Arc::new(|| maho_ai::utils::diagnostics::now_ms().max(0) as u64),
            crate::availability::AMBIENT_STATUS_TTL_MS,
        );
        Self {
            store,
            settings,
            ambient,
            flow: Arc::new(maho_ai::auth::oauth::anthropic::AnthropicOAuth::new(
                maho_ai::auth::oauth::transport::default_transport(),
            )),
        }
    }
    async fn environment(ctx: &dyn AuthContext) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        for name in std::iter::once("CLAUDE_CODE_OAUTH_TOKEN".to_owned())
            .chain((2..=16).map(|i| format!("CLAUDE_CODE_OAUTH_TOKEN_{i}")))
        {
            if let Some(value) = ctx.env(&name).await {
                environment.insert(name, value);
            }
        }
        environment
    }
    async fn configured(
        &self,
        credential: Option<&OAuthCredential>,
        environment: &BTreeMap<String, String>,
        signal: &AbortSignal,
    ) -> anyhow::Result<bool> {
        let settings = (self.settings)();
        let accounts = credential
            .map(|credential| list_accounts(credential, None))
            .transpose()?
            .unwrap_or_default();
        let selected = credential.is_some_and(|credential| {
            credential.access != SENTINEL_TOKEN && credential.refresh != SENTINEL_TOKEN
        });
        let environment_count = environment
            .values()
            .filter(|value| !value.is_empty())
            .count();
        let count = accounts.len() + usize::from(selected) + environment_count;
        let lane = settings
            .values
            .get("tokenInjection")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(if count > 0 { "oauth-slots" } else { "ambient" });
        if lane != "ambient" {
            return Ok(count > 0);
        }
        if environment_count > 0 {
            return Ok(true);
        }
        if settings.values.get("enabled") != Some(&serde_json::Value::Bool(true)) {
            return Ok(false);
        }
        self.ambient.read(Some(signal)).await
    }
    /// senpi `resolveAmbient`; also exposed through `OAuthAuth::resolve_ambient`.
    pub async fn resolve_ambient(
        &self,
        ctx: &dyn AuthContext,
        request: Option<&BTreeMap<String, String>>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        let mut environment: BTreeMap<String, String> = request
            .into_iter()
            .flat_map(|environment| environment.iter())
            .filter(|(name, _)| {
                name.as_str() == "CLAUDE_CODE_OAUTH_TOKEN"
                    || (2..=16).any(|i| name.as_str() == format!("CLAUDE_CODE_OAUTH_TOKEN_{i}"))
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if environment.is_empty() {
            environment = Self::environment(ctx).await;
        }
        if !self.configured(None, &environment, signal).await? {
            return Ok(None);
        }
        Ok(Some(AuthResult {
            ambient: false,
            auth: ModelAuth {
                api_key: Some(SENTINEL_TOKEN.into()),
                ..Default::default()
            },
            env: (!environment.is_empty()).then_some(environment),
            source: Some("Anthropic Subscription".into()),
        }))
    }
}

fn recovery_target(accounts: &[AccountSlot]) -> Option<&str> {
    if accounts.len() == 1 {
        return Some(&accounts[0].name);
    }
    let mut blocked = accounts
        .iter()
        .filter(|slot| slot.block_reason.as_deref() == Some("auth_error"));
    let target = blocked.next()?;
    blocked.next().is_none().then_some(target.name.as_str())
}
fn to_slot(credential: OAuthCredential, name: String, source: AccountSource) -> AccountSlot {
    AccountSlot {
        name,
        display_name: None,
        access: credential.access,
        refresh: credential.refresh,
        expires: credential.expires,
        source,
        blocked_until: None,
        block_reason: None,
    }
}

/// Apply only the login's changed slots to the latest stored pool.
pub(crate) fn merge_login(
    current: Option<Credential>,
    before: &OAuthCredential,
    logged: OAuthCredential,
) -> anyhow::Result<OAuthCredential> {
    let previous = list_accounts(before, None)?;
    let mut current = match current {
        Some(Credential::OAuth(current)) => current,
        Some(Credential::ApiKey(_)) => anyhow::bail!("Provider account management requires an OAuth credential"),
        None => empty_credential(),
    };
    for slot in list_accounts(&logged, None)? {
        if !previous.iter().any(|old| old == &slot) {
            current = upsert_account(&current, slot)?;
        }
    }
    Ok(current)
}

#[async_trait::async_trait]
impl OAuthAuth for AnthropicSubscriptionOAuth {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }
    fn is_subscription(&self) -> bool {
        true
    }
    fn has_resolve_ambient(&self) -> bool {
        true
    }
    async fn resolve_ambient(
        &self,
        ctx: &dyn AuthContext,
        env: Option<&BTreeMap<String, String>>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        AnthropicSubscriptionOAuth::resolve_ambient(self, ctx, env, signal).await
    }
    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        let current = match self
            .store
            .read(crate::auth_lane::PROVIDER_ID, None)
            .await?
        {
            Some(Credential::OAuth(current)) => current,
            Some(Credential::ApiKey(_)) => anyhow::bail!("Provider account management requires an OAuth credential"),
            None => empty_credential(),
        };
        let existing = list_accounts(&current, None)?;
        if existing.is_empty()
            && let Some(imported) = self
                .store
                .read("anthropic", None)
                .await?
                .and_then(Credential::into_oauth)
        {
            let answer = interaction.prompt(AuthPrompt { kind: AuthPromptKind::Text { message: "An Anthropic OAuth login already exists. Move it here (the anthropic provider is logged out) instead of a new login? [y/N]".into(), placeholder: None }, signal: Some(interaction.signal.clone()) }).await?;
            if ["y", "yes"].contains(&answer.trim().to_lowercase().as_str()) {
                self.store.delete("anthropic", None).await?;
                return add_account(
                    &current,
                    to_slot(imported, "imported-anthropic".into(), AccountSource::Import),
                );
            }
        }
        let credential = self.flow.login(interaction).await?;
        let name = if existing.is_empty() {
            "default".into()
        } else {
            let recovery = recovery_target(&existing);
            let fallback = format!("account-{}", existing.len() + 1);
            let names = existing
                .iter()
                .map(|slot| slot.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let message = match recovery {
                Some(target) => format!(
                    "Name for this account (existing: {names}; press Enter to refresh '{target}' with this login)"
                ),
                None => format!(
                    "Name for this account (existing: {names}; press Enter to add {fallback}, or type an existing name to refresh it)"
                ),
            };
            let answer = interaction
                .prompt(AuthPrompt {
                    kind: AuthPromptKind::Text {
                        message,
                        placeholder: Some(recovery.unwrap_or(&fallback).into()),
                    },
                    signal: Some(interaction.signal.clone()),
                })
                .await?;
            if answer.trim().is_empty() {
                recovery.unwrap_or(&fallback).into()
            } else {
                answer.trim().into()
            }
        };
        upsert_account(&current, to_slot(credential, name, AccountSource::Login))
    }
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        Ok(credential.clone())
    }
    async fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&OAuthCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        let environment = Self::environment(ctx).await;
        Ok(self
            .configured(credential, &environment, signal)
            .await?
            .then(|| AuthCheck {
                ambient: false,
                source: Some("Anthropic Subscription".into()),
                auth_type: AuthType::OAuth,
            }))
    }
    async fn to_auth(&self, _credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(SENTINEL_TOKEN.into()),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_merge_preserves_latest_siblings_pinning_and_provider_metadata() {
        let before = add_account(&empty_credential(), to_slot(OAuthCredential::new("old", "r", 1.0), "default".into(), AccountSource::Login)).expect("initial slot");
        let mut sibling = to_slot(OAuthCredential::new("sibling", "r", 2.0), "work".into(), AccountSource::Import);
        sibling.display_name = Some("Work".into());
        sibling.block_reason = Some("rate_limit".into());
        sibling.blocked_until = Some(500.0);
        let current = add_account(&before, sibling.clone()).expect("concurrent sibling")
            .with_extra("pinned", serde_json::json!("work"))
            .with_extra("providerMetadata", serde_json::json!({"revision": 2}));
        let logged = upsert_account(&before, to_slot(OAuthCredential::new("fresh", "rotated", 1000.0), "default".into(), AccountSource::Login)).expect("login result");

        let merged = merge_login(Some(Credential::OAuth(current.clone())), &before, logged).expect("merge");

        let accounts = list_accounts(&merged, None).expect("accounts");
        assert_eq!(accounts[0].access, "fresh");
        assert_eq!(accounts[1], sibling);
        assert_eq!(merged.get_extra_str("pinned"), Some("work"));
        assert_eq!(merged.get_extra("providerMetadata"), current.get_extra("providerMetadata"));
        crate::accounts::assert_sentinel_invariant(&merged).expect("sentinel");
    }

    #[test]
    fn login_merge_does_not_restore_a_removed_unchanged_slot() {
        let before = add_account(&empty_credential(), to_slot(OAuthCredential::new("old", "r", 1.0), "default".into(), AccountSource::Login)).expect("initial slot");
        let logged = add_account(&before, to_slot(OAuthCredential::new("new", "r", 2.0), "work".into(), AccountSource::Login)).expect("login result");

        let merged = merge_login(Some(Credential::OAuth(empty_credential())), &before, logged).expect("merge");

        assert_eq!(list_accounts(&merged, None).expect("accounts").iter().map(|slot| slot.name.as_str()).collect::<Vec<_>>(), ["work"]);
    }
}
