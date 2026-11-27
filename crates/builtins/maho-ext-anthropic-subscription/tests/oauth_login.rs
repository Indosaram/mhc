use maho_ai::{
    auth::{credential_store::InMemoryCredentialStore, types::*},
    utils::abort::AbortController,
};
use maho_ext_anthropic_subscription::{
    accounts::*, availability::AmbientAuthStatusReader, oauth_login::AnthropicSubscriptionOAuth,
};
use std::sync::Arc;

#[tokio::test]
#[cfg(unix)]
async fn native_oauth_constructor_probes_configured_executable() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().expect("dir");
    let executable = directory.path().join("claude");
    std::fs::write(
        &executable,
        "#!/bin/sh\n[ \"$1\" = auth ] && [ \"$2\" = status ]\n",
    )
    .expect("script");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");
    let settings = Arc::new(|| {
        maho_ext_anthropic_subscription::settings::load(
            &serde_json::json!({"anthropicSubscriptionProvider":{"enabled":true}}),
            &serde_json::Value::Null,
            &Default::default(),
        )
    });
    let oauth = AnthropicSubscriptionOAuth::native(
        Arc::new(InMemoryCredentialStore::new()),
        settings,
        executable,
    );
    let result = oauth
        .check(&Context, None, &AbortController::new().signal())
        .await
        .expect("check");
    assert!(result.is_some());
}
struct Interaction(&'static str);
#[async_trait::async_trait]
impl AuthInteraction for Interaction {
    fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> {
        None
    }
    async fn prompt(&self, _: AuthPrompt) -> anyhow::Result<String> {
        Ok(self.0.into())
    }
    fn notify(&self, _: AuthEvent) {}
}
struct Context;
#[async_trait::async_trait]
impl AuthContext for Context {
    async fn env(&self, _: &str) -> Option<String> {
        None
    }
    async fn file_exists(&self, _: &str) -> bool {
        false
    }
}
fn config(store: Arc<InMemoryCredentialStore>) -> AnthropicSubscriptionOAuth {
    let mut oauth = AnthropicSubscriptionOAuth::native_with_resolver(
        store,
        Arc::new(Default::default),
        Arc::new(|| anyhow::bail!("no CLI in the auth check fixture")),
    );
    oauth.ambient = AmbientAuthStatusReader::new(
        Arc::new(|| Box::pin(async { panic!("ambient probe requires opt-in") })),
        Arc::new(|| 0),
        30_000,
    );
    oauth
}
#[tokio::test]
async fn request_auth_for_the_managed_pool_keeps_sentinel() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    let credential = empty_credential();
    assert_eq!(
        oauth
            .to_auth(&credential)
            .await
            .expect("auth")
            .api_key
            .as_deref(),
        Some(SENTINEL_TOKEN)
    );
}
#[tokio::test]
async fn pre_cancelled_login_never_resolves_executable() {
    let controller = AbortController::new();
    controller.abort(None);
    let input = ProviderAuthInteraction::new(controller.signal(), Arc::new(Interaction("")));
    let mut oauth = config(Arc::new(InMemoryCredentialStore::new()));
    oauth.cli.resolve = Arc::new(|| panic!("pre-cancelled login spawned"));
    assert!(oauth.login(&input).await.is_err());
}
#[tokio::test]
async fn check_rejects_a_flat_legacy_token_credential() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    assert!(
        oauth
            .check(
                &Context,
                Some(&OAuthCredential::new("access", "refresh", 60_000.0)),
                &AbortController::new().signal()
            )
            .await
            .expect("check")
            .is_none()
    );
}
#[tokio::test]
async fn check_rejects_a_stored_slot_from_retired_token_login() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    let retired = add_account(
        &empty_credential(),
        AccountSlot {
            name: "old".into(),
            access: "fake-a".into(),
            refresh: "fake-r".into(),
            expires: 1.0,
            ..Default::default()
        },
    )
    .expect("retired credential fixture");
    assert!(
        oauth
            .check(&Context, Some(&retired), &AbortController::new().signal())
            .await
            .expect("auth check")
            .is_none()
    );
}

#[tokio::test]
async fn auth_check_reads_only_anthropic_tokens_from_request_environment() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    let irrelevant = empty_credential().with_extra("env", serde_json::json!({"PATH": "/fixture"}));
    let with_token = empty_credential().with_extra(
        "env",
        serde_json::json!({"PATH": "/fixture", "CLAUDE_CODE_OAUTH_TOKEN_3": "fixture"}),
    );
    let signal = AbortController::new().signal();
    assert!(
        oauth
            .check(&Context, Some(&irrelevant), &signal)
            .await
            .expect("unrelated env check")
            .is_none()
    );
    assert!(
        oauth
            .check(&Context, Some(&with_token), &signal)
            .await
            .expect("request token check")
            .is_some()
    );
}
#[tokio::test]
async fn check_rejects_projected_sentinel_without_accounts() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    assert!(
        oauth
            .check(
                &Context,
                Some(&empty_credential()),
                &AbortController::new().signal()
            )
            .await
            .expect("check")
            .is_none()
    );
}
#[tokio::test]
async fn ambient_request_projects_only_oauth_environment() {
    let oauth = config(Arc::new(InMemoryCredentialStore::new()));
    let request = [
        ("CLAUDE_CODE_OAUTH_TOKEN".into(), "synthetic".into()),
        ("PATH".into(), "/untrusted".into()),
    ]
    .into();
    let resolved = oauth
        .resolve_ambient(&Context, Some(&request), &AbortController::new().signal())
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.auth.api_key.as_deref(), Some(SENTINEL_TOKEN));
    let environment = resolved.env.expect("environment");
    assert_eq!(environment.len(), 1);
    assert!(environment.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
}
#[tokio::test]
async fn ambient_cli_requires_explicit_opt_in_and_shares_check_predicate() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut oauth = config(Arc::new(InMemoryCredentialStore::new()));
    oauth.ambient = AmbientAuthStatusReader::new(
        {
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(true) })
            })
        },
        Arc::new(|| 0),
        30_000,
    );
    let signal = AbortController::new().signal();
    assert!(
        oauth
            .resolve_ambient(&Context, None, &signal)
            .await
            .expect("unconfigured")
            .is_none()
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    oauth.settings = Arc::new(|| {
        maho_ext_anthropic_subscription::settings::load(
            &serde_json::json!({"anthropicSubscriptionProvider":{"enabled":true}}),
            &serde_json::Value::Null,
            &Default::default(),
        )
    });
    assert!(
        oauth
            .check(&Context, None, &signal)
            .await
            .expect("check")
            .is_some()
    );
    assert!(
        oauth
            .resolve_ambient(&Context, None, &signal)
            .await
            .expect("resolve")
            .is_some()
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
