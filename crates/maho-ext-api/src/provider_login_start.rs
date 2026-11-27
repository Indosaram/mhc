use crate::{ExtensionFuture, ProviderLoginProgress, ProviderLoginStartEvent};
use maho_ai::auth::types::{
    AuthCheck, AuthContext, AuthEvent, AuthResult, LoginOptions, LogoutOptions, ModelAuth,
    OAuthAuth, OAuthCredential, OAuthRevocationResult, ProviderAuthInteraction,
};
use maho_ai::types::ProviderEnv;
use maho_ai::utils::abort::AbortSignal;
use std::sync::Arc;

pub type ProviderLoginStartEmitter =
    Arc<dyn Fn(ProviderLoginStartEvent) -> ExtensionFuture<'static, ()> + Send + Sync>;

/// Wraps a registered provider's OAuth login so `provider_login_start` is awaited before the
/// provider's own login code runs (senpi#4438 `withProviderLoginStart`).
pub fn with_provider_login_start(
    provider: &str,
    oauth: Arc<dyn OAuthAuth>,
    emit: ProviderLoginStartEmitter,
) -> Arc<dyn OAuthAuth> {
    Arc::new(ProviderLoginStartOAuth {
        provider: provider.to_owned(),
        inner: oauth,
        emit,
    })
}

struct ProviderLoginStartOAuth {
    provider: String,
    inner: Arc<dyn OAuthAuth>,
    emit: ProviderLoginStartEmitter,
}

#[async_trait::async_trait]
impl OAuthAuth for ProviderLoginStartOAuth {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn is_subscription(&self) -> bool {
        self.inner.is_subscription()
    }
    fn login_label(&self) -> Option<&str> {
        self.inner.login_label()
    }
    fn rejected_token_statuses(&self) -> &[u16] {
        self.inner.rejected_token_statuses()
    }
    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        self.emit_start(interaction).await;
        self.inner.login(interaction).await
    }
    async fn login_with_options(
        &self,
        interaction: &ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> anyhow::Result<OAuthCredential> {
        self.emit_start(interaction).await;
        self.inner.login_with_options(interaction, options).await
    }
    fn account_identity(&self, access: &str) -> Option<String> {
        self.inner.account_identity(access)
    }
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        self.inner.refresh(credential, signal).await
    }
    fn has_revoke(&self) -> bool {
        self.inner.has_revoke()
    }
    async fn revoke(
        &self,
        credential: &OAuthCredential,
        options: Option<LogoutOptions>,
    ) -> anyhow::Result<OAuthRevocationResult> {
        self.inner.revoke(credential, options).await
    }
    async fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&OAuthCredential>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        self.inner.check(ctx, credential, signal).await
    }
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        self.inner.to_auth(credential).await
    }
    fn has_resolve_ambient(&self) -> bool {
        self.inner.has_resolve_ambient()
    }
    async fn resolve_ambient(
        &self,
        ctx: &dyn AuthContext,
        env: Option<&ProviderEnv>,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>> {
        self.inner.resolve_ambient(ctx, env, signal).await
    }
}

impl ProviderLoginStartOAuth {
    async fn emit_start(&self, interaction: &ProviderAuthInteraction) {
        let notify = interaction.clone();
        let progress = ProviderLoginProgress::new(Arc::new(move |message: &str| {
            notify.notify(AuthEvent::Progress {
                message: message.to_owned(),
            });
        }));
        let event = ProviderLoginStartEvent {
            provider: self.provider.clone(),
            signal: interaction.signal.clone(),
            progress,
        };
        let _ignored = (self.emit)(event).await;
    }
}
