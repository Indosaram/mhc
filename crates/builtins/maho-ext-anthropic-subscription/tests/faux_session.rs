use std::sync::Arc;
use maho_ai::auth::{credential_store::InMemoryCredentialStore, types::{Credential, CredentialStore}};
use maho_ext_anthropic_subscription::{accounts::{AccountSlot, AccountSource, add_account, empty_credential}, extension::AnthropicSubscriptionExtension, oauth_login::AnthropicSubscriptionOAuth};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};

#[tokio::test]
async fn native_builtin_command_pins_and_removes_accounts_through_bound_session() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let credential = add_account(&empty_credential(), AccountSlot { name: "work".into(), display_name: None, access: "synthetic".into(), refresh: "synthetic".into(), expires: 999999.0, source: AccountSource::Login, blocked_until: None, block_reason: None }).expect("slot");
    store.modify("anthropic-subscription", Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })), None).await.expect("seed");
    for prompt in ["/claude-account rename work Primary account", "/claude-account clear-name work", "/claude-account pin work", "/claude-account remove work"] {
        let settings = Arc::new(|| maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}), &serde_json::Value::Null, &Default::default()));
        let oauth = Arc::new(AnthropicSubscriptionOAuth { store: store.clone(), flow: Arc::new(maho_ai::auth::oauth::anthropic::AnthropicOAuth::new(maho_ai::auth::oauth::transport::default_transport())), settings: settings.clone(), ambient: maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(|| Box::pin(async { Ok(false) })), Arc::new(|| 1), 30_000) });
        let session = FauxSession::new(FauxScript { name: "anthropic-account".into(), prompt: prompt.into(), responses: Vec::new() }).with_native_extension(NativeExtensionFactory { path: "<anthropic-subscription>".into(), source_info: Default::default(), extension: Box::new(AnthropicSubscriptionExtension { oauth, settings, registry: Arc::new(tokio::sync::Mutex::new(Default::default())), stream: Arc::new(|_, _, _| panic!("account command must not stream")) }) });
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded command").expect("native builtin");
        assert_eq!(result["messages"], serde_json::json!([]));
        let credential = store.read("anthropic-subscription", None).await.expect("read").expect("credential").into_oauth().expect("oauth");
        if prompt.contains("rename work") { assert_eq!(credential.extra["accounts"][0]["displayName"], "Primary account"); }
        else if prompt.contains("clear-name") { assert!(credential.extra["accounts"][0].get("displayName").is_none()); }
        else if prompt.contains("pin work") { assert_eq!(credential.get_extra_str("pinned"), Some("work")); } else { assert!(credential.get_extra_str("pinned").is_none()); assert!(maho_ext_anthropic_subscription::accounts::list_accounts(&credential, None).expect("slots").is_empty()); }
    }
}
