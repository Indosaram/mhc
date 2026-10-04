use std::sync::Arc;
use maho_ai::{auth::{credential_store::InMemoryCredentialStore, types::*}, utils::abort::AbortController};
use maho_ext_anthropic_subscription::{accounts::*, availability::AmbientAuthStatusReader, oauth_login::AnthropicSubscriptionOAuth};

struct Flow;
#[tokio::test]
async fn native_oauth_constructor_probes_configured_executable() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("dir"); let executable = directory.path().join("claude");
    std::fs::write(&executable,"#!/bin/sh\n[ \"$1\" = auth ] && [ \"$2\" = status ]\n").expect("script");
    std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let settings = Arc::new(|| maho_ext_anthropic_subscription::settings::load(&serde_json::json!({"anthropicSubscriptionProvider":{"enabled":true}}),&serde_json::Value::Null,&Default::default()));
    let oauth = AnthropicSubscriptionOAuth::native(Arc::new(InMemoryCredentialStore::new()),settings,executable);
    let result = oauth.check(&Context,None,&AbortController::new().signal()).await.expect("check");
    assert!(result.is_some());
}
#[async_trait::async_trait]
impl OAuthAuth for Flow {
    fn name(&self) -> &str { "synthetic" }
    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        interaction.signal.throw_if_aborted()?;
        Ok(OAuthCredential::new("fresh", "refresh", 60_000.0))
    }
    async fn refresh(&self, credential: &OAuthCredential, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<OAuthCredential> { Ok(credential.clone()) }
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> { Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() }) }
}
struct Interaction(&'static str);
#[async_trait::async_trait]
impl AuthInteraction for Interaction {
    fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> { None }
    async fn prompt(&self, _: AuthPrompt) -> anyhow::Result<String> { Ok(self.0.into()) }
    fn notify(&self, _: AuthEvent) {}
}
struct Context;
#[async_trait::async_trait]
impl AuthContext for Context {
    async fn env(&self, _: &str) -> Option<String> { None }
    async fn file_exists(&self, _: &str) -> bool { false }
}
fn config(store: Arc<InMemoryCredentialStore>) -> AnthropicSubscriptionOAuth {
    AnthropicSubscriptionOAuth { store, flow: Arc::new(Flow), settings: Arc::new(|| maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}), &serde_json::Value::Null, &Default::default())), ambient: AmbientAuthStatusReader::new(Arc::new(|| Box::pin(async { panic!("ambient probe requires opt-in") })), Arc::new(|| 0), 30_000) }
}
fn interaction(answer: &'static str) -> ProviderAuthInteraction { ProviderAuthInteraction::new(AbortController::new().signal(), Arc::new(Interaction(answer))) }
async fn seed(store: &InMemoryCredentialStore, provider: &str, credential: OAuthCredential) {
    store.modify(provider, Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })), None).await.expect("seed");
}
#[tokio::test]
async fn import_moves_existing_grant_without_fresh_login() {
    let store = Arc::new(InMemoryCredentialStore::new()); seed(&store, "anthropic", OAuthCredential::new("imported", "refresh", 1.0)).await;
    let credential = config(store.clone()).login(&interaction("y")).await.expect("login");
    let slots = list_accounts(&credential, None).expect("slots"); assert_eq!(slots[0].name, "imported-anthropic"); assert_eq!(slots[0].source, AccountSource::Import); assert!(store.read("anthropic", None).await.expect("read").is_none());
}
#[tokio::test]
async fn declined_import_keeps_original_and_creates_default() {
    let store = Arc::new(InMemoryCredentialStore::new()); seed(&store, "anthropic", OAuthCredential::new("imported", "refresh", 1.0)).await;
    let credential = config(store.clone()).login(&interaction("n")).await.expect("login");
    let slots = list_accounts(&credential, None).expect("slots"); assert_eq!(slots[0].name, "default"); assert_eq!(slots[0].source, AccountSource::Login); assert!(store.read("anthropic", None).await.expect("read").is_some());
}
#[tokio::test]
async fn second_login_adds_named_account_without_dropping_slots() {
    let store = Arc::new(InMemoryCredentialStore::new()); let oauth = config(store.clone()); let first = oauth.login(&interaction("")).await.expect("first"); seed(&store, "anthropic-subscription", first).await;
    let second = oauth.login(&interaction("work")).await.expect("second"); assert_eq!(list_accounts(&second, None).expect("slots").iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["default", "work"]);
}
#[tokio::test]
async fn login_and_request_auth_keep_managed_sentinel() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new())); let credential = oauth.login(&interaction("")).await.expect("login"); assert_sentinel_invariant(&credential).expect("sentinel"); assert_eq!(oauth.to_auth(&credential).await.expect("auth").api_key.as_deref(), Some(SENTINEL_TOKEN));
}
#[tokio::test]
async fn login_passes_abort_signal_to_underlying_flow() {
    let controller = AbortController::new(); controller.abort(None); let input = ProviderAuthInteraction::new(controller.signal(), Arc::new(Interaction("")));
    assert!(config(Arc::new(InMemoryCredentialStore::new())).login(&input).await.is_err());
}
#[tokio::test]
async fn refresh_preserves_pool_and_sentinel() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new())); let credential = oauth.login(&interaction("")).await.expect("login"); assert_eq!(oauth.refresh(&credential, &AbortController::new().signal()).await.expect("refresh"), credential);
}
#[tokio::test]
async fn check_accepts_projected_concrete_slot() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new())); assert!(oauth.check(&Context, Some(&OAuthCredential::new("access", "refresh", 60_000.0)), &AbortController::new().signal()).await.expect("check").is_some());
}
#[tokio::test]
async fn check_rejects_projected_sentinel_without_accounts() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new())); assert!(oauth.check(&Context, Some(&empty_credential()), &AbortController::new().signal()).await.expect("check").is_none());
}
#[tokio::test]
async fn ambient_request_projects_only_oauth_environment() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    let request = [("CLAUDE_CODE_OAUTH_TOKEN".into(), "synthetic".into()), ("PATH".into(), "/untrusted".into())].into();
    let resolved = oauth.resolve_ambient(&Context, Some(&request), &AbortController::new().signal()).await.expect("resolve").expect("configured");
    assert_eq!(resolved.auth.api_key.as_deref(), Some(SENTINEL_TOKEN));
    let environment = resolved.env.expect("environment"); assert_eq!(environment.len(), 1); assert!(environment.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
}
#[tokio::test]
async fn ambient_cli_requires_explicit_opt_in_and_shares_check_predicate() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut oauth = config(Arc::new(InMemoryCredentialStore::new()));
    oauth.ambient = AmbientAuthStatusReader::new({ let calls = calls.clone(); Arc::new(move || { calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Box::pin(async { Ok(true) }) }) }, Arc::new(|| 0), 30_000);
    let signal = AbortController::new().signal();
    assert!(oauth.resolve_ambient(&Context, None, &signal).await.expect("unconfigured").is_none()); assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    oauth.settings = Arc::new(|| maho_ext_anthropic_subscription::settings::load(&serde_json::json!({"anthropicSubscriptionProvider":{"enabled":true}}), &serde_json::Value::Null, &Default::default()));
    assert!(oauth.check(&Context, None, &signal).await.expect("check").is_some()); assert!(oauth.resolve_ambient(&Context, None, &signal).await.expect("resolve").is_some()); assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
