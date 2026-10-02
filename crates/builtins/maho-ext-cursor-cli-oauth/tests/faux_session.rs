use std::sync::Arc;
use maho_ai::auth::{credential_store::InMemoryCredentialStore, types::{Credential, CredentialStore}};
use maho_ext_cursor_cli_oauth::{accounts::{CursorCliAccountSlot, AccountSource, add_account, empty_credential}, extension::CursorCliExtension, oauth_login::CursorCliOAuth, settings::CursorCliOauthProviderSettings};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};

#[tokio::test]
async fn native_builtin_account_commands_mutate_bound_credentials_without_streaming() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let credential = add_account(&empty_credential(), CursorCliAccountSlot { name: "work".into(), display_name: None, access: "synthetic".into(), refresh: "synthetic".into(), expires: 999999.0, source: AccountSource::Login, blocked_until: None, block_reason: None }).expect("slot");
    store.modify("cursor-cli-oauth", Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })), None).await.expect("seed");
    store.modify("cursor", Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(maho_ai::auth::types::OAuthCredential::new("fixture", "fixture", 1000.0)))) })), None).await.expect("native seed");
    for prompt in ["/cursor-account rename work Primary account", "/cursor-account clear-name work", "/cursor-account pin work", "/cursor-account remove work", "/cursor-account import native"] {
        let settings = Arc::new(CursorCliOauthProviderSettings::default);
        let oauth = Arc::new(CursorCliOAuth { store: store.clone(), flow: Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()), settings: settings.clone(), resolve: Arc::new(|_| Ok(())), persist_acknowledgement: Arc::new(|_| Ok(())), persist_enabled: Arc::new(|_| Ok(())), now: Arc::new(|| 1) });
        let session = FauxSession::new(FauxScript { name: "cursor-account".into(), prompt: prompt.into(), responses: Vec::new() }).with_native_extension(NativeExtensionFactory { path: "<cursor-cli-oauth>".into(), source_info: Default::default(), extension: Box::new(CursorCliExtension { oauth, settings, stream: Arc::new(|_, _, _| panic!("command must not stream")) }) });
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded command").expect("native builtin");
        assert_eq!(result["messages"], serde_json::json!([]));
        let credential = store.read("cursor-cli-oauth", None).await.expect("read").expect("credential").into_oauth().expect("oauth");
        if prompt.contains("rename work") { assert_eq!(credential.extra["accounts"][0]["displayName"], "Primary account"); }
        else if prompt.contains("clear-name") { assert!(credential.extra["accounts"][0].get("displayName").is_none()); }
        else if prompt.contains("import native") { assert_eq!(maho_ext_cursor_cli_oauth::accounts::list_accounts(&credential).expect("slots")[0].name,"native"); }
        else if prompt.contains("pin work") { assert_eq!(credential.get_extra_str("pinned"), Some("work")); } else { assert!(credential.get_extra_str("pinned").is_none()); assert!(maho_ext_cursor_cli_oauth::accounts::list_accounts(&credential).expect("slots").is_empty()); }
    }
}
