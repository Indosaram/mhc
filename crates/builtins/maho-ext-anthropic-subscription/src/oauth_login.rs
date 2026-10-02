use std::{collections::BTreeMap, sync::Arc};
use maho_ai::{auth::types::{AuthCheck, AuthContext, AuthPrompt, AuthPromptKind, AuthResult, AuthType, Credential, CredentialStore, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction}, utils::abort::AbortSignal};
use crate::accounts::{AccountSlot, AccountSource, SENTINEL_TOKEN, add_account, empty_credential, list_accounts, upsert_account};

pub const PROVIDER_NAME: &str = "Anthropic Subscription (Claude Pro/Max)";
pub struct AnthropicSubscriptionOAuth {
    pub store: Arc<dyn CredentialStore>,
    pub flow: Arc<dyn OAuthAuth>,
    pub settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
    pub ambient: Arc<crate::availability::AmbientAuthStatusReader>,
}

impl AnthropicSubscriptionOAuth {
    async fn environment(ctx: &dyn AuthContext) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        for name in std::iter::once("CLAUDE_CODE_OAUTH_TOKEN".to_owned()).chain((2..=16).map(|i| format!("CLAUDE_CODE_OAUTH_TOKEN_{i}"))) {
            if let Some(value) = ctx.env(&name).await { environment.insert(name, value); }
        }
        environment
    }
    async fn configured(&self, credential: Option<&OAuthCredential>, environment: &BTreeMap<String, String>, signal: &AbortSignal) -> anyhow::Result<bool> {
        let settings = (self.settings)();
        let accounts = credential.map(|credential| list_accounts(credential, None)).transpose()?.unwrap_or_default();
        let selected = credential.is_some_and(|credential| credential.access != SENTINEL_TOKEN && credential.refresh != SENTINEL_TOKEN);
        let environment_count = environment.values().filter(|value| !value.is_empty()).count();
        let count = accounts.len() + usize::from(selected) + environment_count;
        let lane = settings.values.get("tokenInjection").and_then(serde_json::Value::as_str).unwrap_or(if count > 0 { "oauth-slots" } else { "ambient" });
        if lane != "ambient" { return Ok(count > 0); }
        if environment_count > 0 { return Ok(true); }
        if settings.values.get("enabled") != Some(&serde_json::Value::Bool(true)) { return Ok(false); }
        self.ambient.read(Some(signal)).await
    }
    /// Ambient resolution remains a concrete method until the shared OAuthAuth
    /// trait exposes the upstream resolveAmbient hook.
    pub async fn resolve_ambient(&self, ctx: &dyn AuthContext, request: Option<&BTreeMap<String, String>>, signal: &AbortSignal) -> anyhow::Result<Option<AuthResult>> {
        let mut environment: BTreeMap<String, String> = request.into_iter().flat_map(|environment| environment.iter()).filter(|(name, _)| name.as_str() == "CLAUDE_CODE_OAUTH_TOKEN" || (2..=16).any(|i| name.as_str() == format!("CLAUDE_CODE_OAUTH_TOKEN_{i}"))).map(|(name, value)| (name.clone(), value.clone())).collect();
        if environment.is_empty() { environment = Self::environment(ctx).await; }
        if !self.configured(None, &environment, signal).await? { return Ok(None); }
        Ok(Some(AuthResult { auth: ModelAuth { api_key: Some(SENTINEL_TOKEN.into()), ..Default::default() }, env: (!environment.is_empty()).then_some(environment), source: Some("Anthropic Subscription".into()) }))
    }
}

fn recovery_target(accounts: &[AccountSlot]) -> Option<&str> {
    if accounts.len() == 1 { return Some(&accounts[0].name); }
    let mut blocked = accounts.iter().filter(|slot| slot.block_reason.as_deref() == Some("auth_error"));
    let target = blocked.next()?;
    blocked.next().is_none().then_some(target.name.as_str())
}
fn to_slot(credential: OAuthCredential, name: String, source: AccountSource) -> AccountSlot {
    AccountSlot { name, display_name: None, access: credential.access, refresh: credential.refresh, expires: credential.expires, source, blocked_until: None, block_reason: None }
}

#[async_trait::async_trait]
impl OAuthAuth for AnthropicSubscriptionOAuth {
    fn name(&self) -> &str { PROVIDER_NAME }
    fn is_subscription(&self) -> bool { true }
    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let current = self.store.read(crate::auth_lane::PROVIDER_ID, None).await?.and_then(Credential::into_oauth).unwrap_or_else(empty_credential);
        let existing = list_accounts(&current, None)?;
        if existing.is_empty() && let Some(imported) = self.store.read("anthropic", None).await?.and_then(Credential::into_oauth) {
                let answer = interaction.prompt(AuthPrompt { kind: AuthPromptKind::Text { message: "An Anthropic OAuth login already exists. Move it here (the anthropic provider is logged out) instead of a new login? [y/N]".into(), placeholder: None }, signal: Some(interaction.signal.clone()) }).await?;
                if ["y", "yes"].contains(&answer.trim().to_lowercase().as_str()) {
                    self.store.delete("anthropic", None).await?;
                    return add_account(&current, to_slot(imported, "imported-anthropic".into(), AccountSource::Import));
                }
        }
        let credential = self.flow.login(interaction).await?;
        let name = if existing.is_empty() { "default".into() } else {
            let recovery = recovery_target(&existing);
            let fallback = format!("account-{}", existing.len() + 1);
            let names = existing.iter().map(|slot| slot.name.as_str()).collect::<Vec<_>>().join(", ");
            let message = match recovery {
                Some(target) => format!("Name for this account (existing: {names}; press Enter to refresh '{target}' with this login)"),
                None => format!("Name for this account (existing: {names}; press Enter to add {fallback}, or type an existing name to refresh it)"),
            };
            let answer = interaction.prompt(AuthPrompt { kind: AuthPromptKind::Text { message, placeholder: Some(recovery.unwrap_or(&fallback).into()) }, signal: Some(interaction.signal.clone()) }).await?;
            if answer.trim().is_empty() { recovery.unwrap_or(&fallback).into() } else { answer.trim().into() }
        };
        upsert_account(&current, to_slot(credential, name, AccountSource::Login))
    }
    async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> { Ok(credential.clone()) }
    async fn check(&self, ctx: &dyn AuthContext, credential: Option<&OAuthCredential>, signal: &AbortSignal) -> anyhow::Result<Option<AuthCheck>> {
        let environment = Self::environment(ctx).await;
        Ok(self.configured(credential, &environment, signal).await?.then(|| AuthCheck { source: Some("Anthropic Subscription".into()), auth_type: AuthType::OAuth }))
    }
    async fn to_auth(&self, _credential: &OAuthCredential) -> anyhow::Result<ModelAuth> { Ok(ModelAuth { api_key: Some(SENTINEL_TOKEN.into()), ..Default::default() }) }
}
