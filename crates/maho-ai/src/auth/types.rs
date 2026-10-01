//! Port of senpi packages/ai/src/auth/types.ts.
//!
//! `Credential` is a type-tagged enum (`"api_key" | "oauth"`) matching the TS discriminated
//! union; extra OAuth fields carried by concrete provider credentials (e.g. github-copilot's
//! `enterpriseUrl`) are preserved via `extra` so round-tripping through JSON keeps unknown keys.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::types::{ProviderEnv, ProviderHeaders};

/// Request auth for a single model request. If a value cannot be expressed as
/// `api_key`, `headers`, or `base_url`, it is provider config, not auth.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelAuth {
    #[serde(skip_serializing_if = "Option::is_none", rename = "apiKey")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "baseUrl")]
    pub base_url: Option<String>,
}

/// Stored api-key credential. `env` holds provider-scoped environment/config values such as
/// Cloudflare account/gateway ids.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ApiKeyCredential {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

/// OAuth token data returned by extension compatibility flows / stored on disk.
///
/// `extra` preserves provider-specific fields (e.g. `enterpriseUrl`) that ride alongside the
/// canonical `refresh`/`access`/`expires` triple, matching the TS `[key: string]: unknown` index
/// signature.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OAuthCredential {
    pub refresh: String,
    pub access: String,
    pub expires: f64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl OAuthCredential {
    pub fn new(access: impl Into<String>, refresh: impl Into<String>, expires: f64) -> Self {
        Self { refresh: refresh.into(), access: access.into(), expires, extra: Map::new() }
    }

    pub fn get_extra(&self, key: &str) -> Option<&Value> {
        self.extra.get(key)
    }

    pub fn get_extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }

    pub fn with_extra(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extra.insert(key.into(), value);
        self
    }
}

/// One type-tagged credential per provider - the shape of today's auth.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl Credential {
    pub fn credential_type(&self) -> CredentialType {
        match self {
            Credential::ApiKey(_) => CredentialType::ApiKey,
            Credential::OAuth(_) => CredentialType::OAuth,
        }
    }

    pub fn as_oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Credential::OAuth(c) => Some(c),
            Credential::ApiKey(_) => None,
        }
    }

    pub fn as_api_key(&self) -> Option<&ApiKeyCredential> {
        match self {
            Credential::ApiKey(c) => Some(c),
            Credential::OAuth(_) => None,
        }
    }

    pub fn into_oauth(self) -> Option<OAuthCredential> {
        match self {
            Credential::OAuth(c) => Some(c),
            Credential::ApiKey(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialType {
    ApiKey,
    OAuth,
}

/// Non-secret credential metadata for account/status enumeration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CredentialInfo {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    #[serde(rename = "type")]
    pub credential_type: CredentialType,
}

/// Optional cancellation for public auth and credential operations.
#[derive(Debug, Clone, Default)]
pub struct AuthOperationOptions {
    pub signal: Option<crate::utils::abort::AbortSignal>,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// App-owned credential storage, keyed by `Provider.id`, one entry per provider.
///
/// `modify` is the only write path, so every mutation is a serialized read-modify-write.
/// Error semantics: `read` resolves `None` for missing entries. Methods reject only on storage
/// failure.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Read the stored credential, possibly expired.
    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>>;

    /// List stored credential metadata without resolving or exposing secrets.
    async fn list(&self, options: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>>;

    /// Serialized write - the only write path. Mutual exclusion per provider id.
    async fn modify(
        &self,
        provider_id: &str,
        f: Box<dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>>;

    /// Remove a credential (logout). Implementations serialize this against `modify`.
    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<()>;
}

/// Environment access for auth resolution. Injectable for tests and browsers.
#[async_trait]
pub trait AuthContext: Send + Sync {
    async fn env(&self, name: &str) -> Option<String>;
    /// Check whether a file exists. Supports a leading `~`.
    async fn file_exists(&self, path: &str) -> bool;
}

/// Result of resolving auth for a model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthResult {
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from credentials and ambient context.
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI: "ANTHROPIC_API_KEY", "OAuth", "~/.aws/credentials".
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthCheck {
    pub source: Option<String>,
    #[allow(clippy::struct_field_names)]
    pub auth_type: AuthType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    OAuth,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthPromptOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

/// Prompt shown to the user during login.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthPromptKind {
    Text { message: String, placeholder: Option<String> },
    Secret { message: String, placeholder: Option<String> },
    Select { message: String, options: Vec<AuthPromptOption> },
    ManualCode { message: String, placeholder: Option<String> },
}

#[derive(Debug, Clone)]
pub struct AuthPrompt {
    pub kind: AuthPromptKind,
    pub signal: Option<crate::utils::abort::AbortSignal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthInfoLink {
    pub url: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthEvent {
    Info { message: String, links: Option<Vec<AuthInfoLink>> },
    AuthUrl { url: String, instructions: Option<String> },
    DeviceCode {
        user_code: String,
        verification_uri: String,
        interval_seconds: Option<f64>,
        expires_in_seconds: Option<f64>,
    },
    Progress { message: String },
}

/// Secret-free receipt emitted only after the account write succeeds.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountLoginReceipt {
    pub provider_id: String,
    pub name: String,
    pub origin: AccountLoginOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLoginOrigin {
    Generated,
    Provider,
}

/// Login interaction callbacks serving both api-key and OAuth flows.
#[async_trait]
pub trait AuthInteraction: Send + Sync {
    fn signal(&self) -> Option<crate::utils::abort::AbortSignal>;
    fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}
    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String>;
    fn notify(&self, event: AuthEvent);
}

/// Normalized interaction passed to provider login implementations: `signal` is mandatory.
pub struct ProviderAuthInteraction {
    pub signal: crate::utils::abort::AbortSignal,
    inner: Arc<dyn AuthInteraction>,
}

impl ProviderAuthInteraction {
    pub fn new(signal: crate::utils::abort::AbortSignal, inner: Arc<dyn AuthInteraction>) -> Self {
        Self { signal, inner }
    }

    pub async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        self.inner.prompt(prompt).await
    }

    pub fn notify(&self, event: AuthEvent) {
        self.inner.notify(event);
    }
}

/// Api-key auth: stored key/provider env plus ambient sources.
#[async_trait]
pub trait ApiKeyAuth: Send + Sync {
    fn name(&self) -> &str;
    /// Ambient compatibility adapter that must not outrank a stored OAuth credential.
    fn ambient_only(&self) -> bool {
        false
    }

    async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<ApiKeyCredential> {
        anyhow::bail!("login not supported")
    }

    fn has_login(&self) -> bool {
        false
    }

    async fn check(
        &self,
        _ctx: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
        _signal: &crate::utils::abort::AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        Ok(None)
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
        signal: &crate::utils::abort::AbortSignal,
    ) -> anyhow::Result<Option<AuthResult>>;
}

/// OAuth auth. The `refresh`/`to_auth` split lets the caller own the locked refresh pattern.
#[async_trait]
pub trait OAuthAuth: Send + Sync {
    fn name(&self) -> &str;
    fn is_subscription(&self) -> bool {
        false
    }
    fn login_label(&self) -> Option<&str> {
        None
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential>;

    /// Exchange the refresh token. Network call; errors on failure (invalid_grant etc.).
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &crate::utils::abort::AbortSignal,
    ) -> anyhow::Result<OAuthCredential>;

    async fn check(
        &self,
        _ctx: &dyn AuthContext,
        _credential: Option<&OAuthCredential>,
        _signal: &crate::utils::abort::AbortSignal,
    ) -> anyhow::Result<Option<AuthCheck>> {
        Ok(None)
    }

    /// Side-effect-free derivation of request auth from a valid credential.
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth>;
}

/// Provider auth. At least one of `api_key`/`oauth` must be present.
#[derive(Default)]
pub struct ProviderAuth {
    pub api_key: Option<Arc<dyn ApiKeyAuth>>,
    pub oauth: Option<Arc<dyn OAuthAuth>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_json_tag_matches_ts() {
        let api_key = Credential::ApiKey(ApiKeyCredential { key: Some("sk".into()), env: None });
        let json = serde_json::to_value(&api_key).unwrap();
        assert_eq!(json["type"], "api_key");
        assert_eq!(json["key"], "sk");

        let oauth = Credential::OAuth(OAuthCredential::new("acc", "ref", 123.0));
        let json = serde_json::to_value(&oauth).unwrap();
        assert_eq!(json["type"], "oauth");
        assert_eq!(json["access"], "acc");
        assert_eq!(json["refresh"], "ref");
        assert_eq!(json["expires"], 123.0);
    }

    #[test]
    fn oauth_credential_preserves_extra_fields() {
        let cred = OAuthCredential::new("a", "r", 1.0).with_extra("enterpriseUrl", Value::String("x.example.com".into()));
        let json = serde_json::to_value(&cred).unwrap();
        assert_eq!(json["enterpriseUrl"], "x.example.com");
        let back: OAuthCredential = serde_json::from_value(json).unwrap();
        assert_eq!(back.get_extra_str("enterpriseUrl"), Some("x.example.com"));
    }
}
