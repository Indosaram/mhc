use crate::accounts::{SENTINEL_TOKEN, stored_slots};
use maho_ai::{
    auth::types::{
        AuthCheck, AuthContext, AuthResult, AuthType, CredentialStore, ModelAuth, OAuthAuth,
        OAuthCredential, ProviderAuthInteraction,
    },
    utils::abort::AbortSignal,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

pub const PROVIDER_NAME: &str = "Anthropic Subscription (Claude Pro/Max)";

fn is_oauth_environment_name(name: &str) -> bool {
    name == "CLAUDE_CODE_OAUTH_TOKEN"
        || (2..=16).any(|i| name == format!("CLAUDE_CODE_OAUTH_TOKEN_{i}"))
}

#[derive(Clone)]
pub struct ClaudeCliLoginContext {
    pub agent_dir: PathBuf,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub resolve: crate::executable::ExecutableResolver,
}

#[derive(Clone)]
pub struct AnthropicSubscriptionOAuth {
    pub store: Arc<dyn CredentialStore>,
    pub settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
    pub ambient: Arc<crate::availability::AmbientAuthStatusReader>,
    pub cli: ClaudeCliLoginContext,
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
        Self::native_with_context(
            store,
            settings,
            ClaudeCliLoginContext {
                agent_dir: maho_core::config::get_agent_dir().into(),
                cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                environment: std::env::vars().collect(),
                resolve,
            },
        )
    }

    pub fn native_with_context(
        store: Arc<dyn CredentialStore>,
        settings: Arc<dyn Fn() -> crate::settings::ProviderSettings + Send + Sync>,
        cli: ClaudeCliLoginContext,
    ) -> Self {
        let resolve = cli.resolve.clone();
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
            cli,
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
            .map(stored_slots)
            .transpose()?
            .unwrap_or_default();
        let environment_count = environment
            .values()
            .filter(|value| !value.is_empty())
            .count();
        let count = accounts
            .iter()
            .filter(|slot| slot.is_claude_cli() && !slot.name.trim().is_empty())
            .count()
            + environment_count;
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
            .filter(|(name, _)| is_oauth_environment_name(name))
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
        crate::oauth_login_cli::login(self, interaction).await
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
        let mut environment: BTreeMap<_, _> = credential
            .and_then(|credential| credential.get_extra("env"))
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, _)| is_oauth_environment_name(name))
            .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
            .collect();
        if environment.is_empty() {
            environment = Self::environment(ctx).await;
        }
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
