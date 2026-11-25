//! Ports the direct cases from senpi's chatgpt-subscription-relogin-same-account.test.ts
//! and the pure placement case from chatgpt-subscription-placement.test.ts (v2026.11.19-5).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use maho_ai::auth::context::default_provider_auth_context;
use maho_ai::auth::credential_store::InMemoryCredentialStore;
use maho_ai::auth::pool::slots::{
    AppendLoginSlotOptions, CredentialSlot, CredentialSlotSource, PooledCredential,
    append_login_slot, append_login_slot_with_options, list_slots, merge_refreshed_slot,
    project_slot, remove_slot,
};
use maho_ai::auth::types::{
    ApiKeyCredential, AuthEvent, AuthInteraction, AuthPrompt, AuthType, Credential,
    CredentialStore, LoginOptions, LogoutOptions, ModelAuth, OAuthAuth, OAuthCredential,
    OAuthRevocationResult, ProviderAuth, ProviderAuthInteraction,
};
use maho_ai::models::{
    AuthResolutionOverrides, CreateModelsOptions, CreateProviderOptions, Models, ModelsErrorCode,
    ModelsLoginOptions, ProviderApi, ProviderModelsAuth, create_models, create_provider,
};
use maho_ai::providers::{
    builtin_api_streams, chatgpt_subscription::chatgpt_subscription_provider,
};
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Value, json};

// Synthetic, unsigned JWTs containing only the claims needed for the identity tests.
fn jwt(auth: &Value, tag: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = URL_SAFE_NO_PAD
        .encode(json!({"https://api.openai.com/auth": auth, "jti": tag}).to_string());
    format!("{header}.{payload}.sig")
}

fn person(user: &str) -> Value {
    json!({
        "chatgpt_account_id": "ws-1",
        "chatgpt_account_user_id": format!("{user}__ws-1"),
        "chatgpt_user_id": user,
    })
}

fn login(auth: &Value, tag: &str, expires: f64) -> OAuthCredential {
    OAuthCredential::new(jwt(auth, tag), format!("refresh-{tag}"), expires)
        .with_extra("accountId", auth["chatgpt_account_id"].clone())
}

fn alice(tag: &str, expires: f64) -> OAuthCredential {
    login(&person("user-a"), tag, expires)
}

fn bob(tag: &str, expires: f64) -> OAuthCredential {
    login(&person("user-b"), tag, expires)
}

fn siwc(credential: OAuthCredential, subject: &str, client: &str) -> OAuthCredential {
    credential
        .with_extra("route", json!("siwc"))
        .with_extra("issuer", json!("https://auth.openai.com"))
        .with_extra("subject", json!(subject))
        .with_extra("clientId", json!(client))
}

fn shipped_oauth() -> Arc<dyn OAuthAuth> {
    chatgpt_subscription_provider()
        .expect("shipped ChatGPT subscription provider")
        .auth()
        .and_then(|auth| auth.oauth.clone())
        .expect("shipped ChatGPT OAuth")
}

fn flat(credential: OAuthCredential) -> PooledCredential {
    PooledCredential::from(Credential::OAuth(credential))
}

fn slot(name: &str, credential: &OAuthCredential) -> CredentialSlot {
    CredentialSlot {
        name: name.into(),
        source: Some(CredentialSlotSource::Login),
        access: Some(credential.access.clone()),
        refresh: Some(credential.refresh.clone()),
        expires: Some(credential.expires),
        extra: credential.extra.clone(),
        ..CredentialSlot::default()
    }
}

fn pooled(credential: OAuthCredential, accounts: Vec<CredentialSlot>) -> PooledCredential {
    PooledCredential {
        accounts: Some(accounts),
        ..flat(credential)
    }
}

fn signed_out(mut credential: OAuthCredential) -> OAuthCredential {
    credential.access.clear();
    credential.refresh.clear();
    credential.expires = 0.0;
    credential.with_extra("signedOut", json!(true))
}

fn relogin(
    stored: PooledCredential,
    fresh: OAuthCredential,
    reauthorize_slot: Option<&str>,
    mut allocated: impl FnMut(&str),
) -> Result<PooledCredential, String> {
    let oauth = shipped_oauth();
    let identify = |access: &str| oauth.account_identity(access);
    append_login_slot_with_options(
        Some(stored),
        flat(fresh),
        |name, _| allocated(name),
        AppendLoginSlotOptions {
            account_identity: Some(&identify),
            reauthorize_slot,
        },
    )
    .map_err(|error| error.to_string())
}

fn slot_names(pooled: &PooledCredential) -> Vec<String> {
    list_slots(Some(pooled))
        .into_iter()
        .map(|slot| slot.name)
        .collect()
}

fn slot_refreshes(pooled: &PooledCredential) -> Vec<(String, Option<String>)> {
    list_slots(Some(pooled))
        .into_iter()
        .map(|slot| (slot.name, slot.refresh))
        .collect()
}

fn assert_account_mismatch(message: &str, slot: &str) {
    assert!(
        message.contains(&format!("different ChatGPT account than '{slot}'")),
        "expected account mismatch for {slot}, got: {message}"
    );
}

#[test]
fn same_person_in_same_workspace_has_one_identity_across_tokens() {
    let oauth = shipped_oauth();
    let old = oauth.account_identity(&alice("old", 1.0).access);
    assert!(old.is_some());
    assert_eq!(old, oauth.account_identity(&alice("new", 2.0).access));
}

#[test]
fn two_members_of_one_team_workspace_are_different_accounts() {
    let oauth = shipped_oauth();
    let first = oauth.account_identity(&alice("a", 1.0).access);
    let second = oauth.account_identity(&bob("b", 1.0).access);
    assert!(first.is_some() && second.is_some());
    assert_ne!(first, second);
}

#[test]
fn user_id_without_member_claim_is_scoped_to_workspace() {
    let oauth = shipped_oauth();
    let identity = |workspace: &str| {
        oauth.account_identity(&jwt(
            &json!({"chatgpt_account_id": workspace, "chatgpt_user_id": "user-a"}),
            workspace,
        ))
    };
    let first = identity("ws-1");
    let second = identity("ws-2");
    assert!(first.is_some() && second.is_some());
    assert_ne!(first, second);
}

#[test]
fn workspace_only_or_malformed_token_has_no_identity() {
    let oauth = shipped_oauth();
    let access = jwt(&json!({"chatgpt_account_id": "ws-1"}), "x");
    assert_eq!(oauth.account_identity(&access), None);
    assert_eq!(oauth.account_identity("not-a-jwt"), None);
}

#[test]
fn different_siwc_tuple_never_replaces_siwc_slot_by_token_identity() {
    let saved = siwc(alice("old", 1.0), "sub-a", "client-1");
    let first = slot("default", &saved);
    let fresh = siwc(alice("new", 2.0), "sub-a", "client-2");

    let next = relogin(
        pooled(saved, vec![first.clone()]),
        fresh.clone(),
        None,
        |_| {},
    )
    .expect("different registrations should coexist");

    assert_eq!(list_slots(Some(&next)), [first, slot("login-2", &fresh)]);
}

#[test]
fn same_route_relogin_keeps_account_fields_on_replaced_slot() {
    let saved = siwc(alice("old", 1.0), "sub-a", "client-1");
    let first = slot("default", &saved);
    let fresh = siwc(alice("new", 2.0), "sub-a", "client-1");

    let next = relogin(pooled(saved, vec![first]), fresh.clone(), None, |_| {})
        .expect("same registration should be replaced");

    assert_eq!(list_slots(Some(&next)), [slot("default", &fresh)]);
}

#[test]
fn returning_siwc_registration_keeps_label_pin_order_and_sibling() {
    let old = siwc(alice("old", 1.0), "sub-a", "client-1");
    let sibling = slot("login-2", &siwc(bob("other", 1.0), "sub-b", "client-2"));
    let mut kept = slot("default", &old);
    kept.display_name = Some("Work".into());
    let mut stored = pooled(old, vec![kept, sibling.clone()]);
    stored.pinned = Some("default".into());
    stored
        .extra
        .insert("order".into(), json!(["login-2", "default"]));
    let fresh = siwc(alice("new", 2.0), "sub-a", "client-1");

    let next = relogin(stored, fresh.clone(), None, |_| {}).expect("replace the active login");

    let mut expected = slot("default", &fresh);
    expected.display_name = Some("Work".into());
    assert_eq!(list_slots(Some(&next)), [expected, sibling]);
    assert_eq!(next.pinned.as_deref(), Some("default"));
    assert_eq!(
        next.extra.get("order"),
        Some(&json!(["login-2", "default"]))
    );
}

#[test]
fn matching_email_does_not_replace_an_incomplete_siwc_or_legacy_identity() {
    let email = json!("work@example.test");
    let mut incomplete =
        siwc(alice("missing", 1.0), "sub-a", "client-1").with_extra("email", email.clone());
    incomplete.extra.remove("subject");
    let legacy = alice("legacy", 1.0).with_extra("email", email.clone());
    let fresh = siwc(alice("fresh", 2.0), "sub-a", "client-1").with_extra("email", email);
    let old_slots = [slot("default", &legacy), slot("login-2", &incomplete)];
    let stored = pooled(legacy, old_slots.to_vec());
    let next = relogin(stored, fresh.clone(), None, |_| {}).expect("append verified identity");
    assert_eq!(
        list_slots(Some(&next)),
        [
            old_slots[0].clone(),
            old_slots[1].clone(),
            slot("login-3", &fresh)
        ]
    );
}

#[test]
fn relogin_never_replaces_a_slot_on_the_other_route() {
    let existing = alice("old", 1.0);
    let incoming = alice("new", 2.0);
    for (existing, incoming) in [
        (
            existing.clone(),
            siwc(incoming.clone(), "sub-a", "client-1"),
        ),
        (siwc(existing, "sub-a", "client-1"), incoming),
    ] {
        let first = slot("default", &existing);
        let next = relogin(
            pooled(existing, vec![first.clone()]),
            incoming.clone(),
            None,
            |_| {},
        )
        .expect("keep a different sign-in route separate");
        assert_eq!(list_slots(Some(&next)), [first, slot("login-2", &incoming)]);
    }
}

#[test]
fn flat_saved_account_is_rewritten_in_place() {
    let next =
        relogin(flat(alice("old", 1.0)), alice("new", 2.0), None, |_| {}).expect("flat relogin");

    assert!(next.accounts.is_none());
    assert_eq!(next.refresh.as_deref(), Some("refresh-new"));
    assert_eq!(next.expires, Some(2.0));
}

#[test]
fn matching_slot_keeps_name_label_and_position_but_drops_stale_blocks() {
    let saved = alice("old", 1.0);
    let mut first = slot("default", &saved);
    first.display_name = Some("Work".into());
    first.extra.remove("accountId");
    first
        .extra
        .insert("blockReason".into(), json!("auth_error"));
    first.extra.insert("blockedUntil".into(), json!(99));
    let mut sibling = slot("login-2", &bob("b", 1.0));
    sibling.extra.remove("accountId");
    let fresh = alice("new", 2.0);
    let mut allocated = Vec::new();

    let next = relogin(
        pooled(saved, vec![first, sibling.clone()]),
        fresh.clone(),
        None,
        |name| allocated.push(name.to_owned()),
    )
    .expect("matching slot relogin");

    let mut replacement = slot("default", &fresh);
    replacement.display_name = Some("Work".into());
    assert_eq!(list_slots(Some(&next)), [replacement, sibling]);
    assert_eq!(allocated, ["default"]);
    assert_eq!(next.access.as_deref(), Some(fresh.access.as_str()));
    assert_eq!(next.refresh.as_deref(), Some("refresh-new"));
    assert_eq!(next.expires, Some(2.0));
}

#[test]
fn older_duplicates_collapse_into_first_and_redirect_pin_and_order() {
    let other = bob("b", 1.0);
    let mut stored = pooled(
        other.clone(),
        vec![
            slot("default", &alice("1", 1.0)),
            slot("login-2", &other),
            slot("login-3", &alice("3", 3.0)),
        ],
    );
    stored.pinned = Some("login-3".into());
    stored
        .extra
        .insert("order".into(), json!(["login-3", "login-2", "default"]));

    let next = relogin(stored, alice("new", 4.0), None, |_| {}).expect("deduplicated relogin");

    assert_eq!(
        slot_refreshes(&next),
        [
            ("default".to_owned(), Some("refresh-new".to_owned())),
            ("login-2".to_owned(), Some("refresh-b".to_owned())),
        ]
    );
    assert_eq!(next.pinned.as_deref(), Some("default"));
    assert_eq!(
        next.extra.get("order"),
        Some(&json!(["default", "login-2"]))
    );
    assert_eq!(next.refresh.as_deref(), Some("refresh-b"));
}

#[test]
fn different_or_unknown_account_identity_appends() {
    let stored = flat(alice("a", 1.0));
    let unknown = login(&json!({"chatgpt_account_id": "ws-1"}), "x", 1.0);

    for fresh in [bob("b", 1.0), unknown] {
        let next = relogin(stored.clone(), fresh, None, |_| {}).expect("new login slot");
        assert_eq!(slot_names(&next), ["default", "login-2"]);
    }
}

struct SyntheticOAuth {
    shipped: Arc<dyn OAuthAuth>,
    logins: Mutex<VecDeque<OAuthCredential>>,
}

#[async_trait]
impl OAuthAuth for SyntheticOAuth {
    fn name(&self) -> &str {
        self.shipped.name()
    }

    fn account_identity(&self, access: &str) -> Option<String> {
        self.shipped.account_identity(access)
    }

    async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.logins
            .lock()
            .expect("synthetic login queue")
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("unexpected synthetic login"))
    }

    async fn refresh(
        &self,
        _: &OAuthCredential,
        _: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        anyhow::bail!("test does not refresh through OAuth")
    }

    fn has_revoke(&self) -> bool {
        true
    }

    async fn revoke(
        &self,
        _: &OAuthCredential,
        _: Option<LogoutOptions>,
    ) -> anyhow::Result<OAuthRevocationResult> {
        Ok(OAuthRevocationResult::Revoked)
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..Default::default()
        })
    }
}

struct NonInteractive;

#[async_trait]
impl AuthInteraction for NonInteractive {
    fn signal(&self) -> Option<AbortSignal> {
        None
    }

    async fn prompt(&self, _: AuthPrompt) -> anyhow::Result<String> {
        anyhow::bail!("test does not prompt")
    }

    fn notify(&self, _: AuthEvent) {}
}

fn models_with_logins(
    id: &str,
    logins: impl IntoIterator<Item = OAuthCredential>,
) -> (Models, Arc<InMemoryCredentialStore>) {
    let store = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        auth: Some(Arc::new(ProviderModelsAuth::new(
            store.clone(),
            Arc::new(default_provider_auth_context()),
        ))),
        ..CreateModelsOptions::default()
    }));
    let provider = create_provider(CreateProviderOptions {
        id: id.into(),
        name: None,
        base_url: Some("https://chatgpt.example/v1".into()),
        headers: None,
        auth: Some(ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(SyntheticOAuth {
                shipped: shipped_oauth(),
                logins: Mutex::new(logins.into_iter().collect()),
            })),
        }),
        models: Vec::new(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-responses")),
        images: Default::default(),
    })
    .expect("synthetic provider");
    models.set_provider(provider);
    (models, store)
}

async fn sign_back_in_after_logout(pooled_storage: bool) {
    let provider = "synthetic-sign-back-in";
    let old = siwc(alice("old", 4_102_444_800_000.0), "sub-a", "client-1");
    let fresh = siwc(alice("fresh", 4_102_444_800_000.0), "sub-a", "client-1");
    let (models, store) = models_with_logins(provider, [fresh.clone()]);
    let credential = if pooled_storage {
        pooled(old.clone(), vec![slot("default", &old)]).to_stored_credential()
    } else {
        Credential::OAuth(old)
    };
    store
        .modify(
            provider,
            Box::new(|_| Box::pin(async { Ok(Some(credential)) })),
            None,
        )
        .await
        .expect("seed registration");

    assert_eq!(
        models
            .logout_with_options(provider, None)
            .await
            .expect("sign out"),
        Some(OAuthRevocationResult::Revoked)
    );
    let overrides = AuthResolutionOverrides::default();
    assert!(
        models
            .get_auth(provider, &overrides)
            .await
            .expect("read after logout")
            .is_none()
    );
    models
        .login(provider, AuthType::OAuth, Arc::new(NonInteractive), None)
        .await
        .expect("sign back in");
    let auth = models
        .get_auth(provider, &overrides)
        .await
        .expect("resolve new session")
        .expect("signed-in session");
    assert_eq!(auth.auth.api_key, Some(fresh.access));
    let stored = store
        .read(provider, None)
        .await
        .expect("stored registration")
        .expect("entry");
    let slots = list_slots(Some(&PooledCredential::from(stored)));
    assert_eq!(slots.len(), if pooled_storage { 2 } else { 1 });
    assert_eq!(
        slots
            .iter()
            .filter(|slot| slot.refresh.as_deref() == Some(fresh.refresh.as_str()))
            .count(),
        1
    );
}

#[tokio::test]
async fn new_login_replaces_a_signed_out_flat_registration() {
    sign_back_in_after_logout(false).await;
}

#[tokio::test]
async fn new_login_becomes_the_resolved_account_after_pooled_logout() {
    sign_back_in_after_logout(true).await;
}

#[tokio::test]
async fn logout_deletes_an_api_key_when_the_provider_also_has_revocation() {
    let provider = "synthetic-api-key-logout";
    let (models, store) = models_with_logins(provider, []);
    store
        .modify(
            provider,
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("synthetic-key".into()),
                        ..Default::default()
                    })))
                })
            }),
            None,
        )
        .await
        .expect("seed API key");

    assert_eq!(
        models
            .logout_with_options(provider, None)
            .await
            .expect("logout"),
        None
    );
    assert!(
        store
            .read(provider, None)
            .await
            .expect("read store")
            .is_none()
    );
}

#[tokio::test]
async fn models_login_twice_as_same_person_keeps_one_account_with_newest_tokens() {
    let id = "chatgpt-relogin";
    let (models, store) = models_with_logins(id, [alice("old", 1.0), alice("new", 2.0)]);
    let interaction = Arc::new(NonInteractive);

    models
        .login(id, AuthType::OAuth, interaction.clone(), None)
        .await
        .expect("first synthetic login");
    models
        .login(id, AuthType::OAuth, interaction, None)
        .await
        .expect("second synthetic login");

    let stored = store
        .read(id, None)
        .await
        .expect("store read")
        .expect("stored login");
    assert_eq!(
        slot_refreshes(&PooledCredential::from(stored)),
        [("default".to_owned(), Some("refresh-new".to_owned()))]
    );
}

#[tokio::test]
async fn models_keeps_two_validated_siwc_accounts_and_replaces_the_returning_registration() {
    let provider = "synthetic-siwc-sign-in";
    let old = siwc(alice("old", 1.0), "sub-a", "client-1");
    let other = siwc(bob("other", 1.0), "sub-b", "client-2");
    let fresh = siwc(alice("fresh", 2.0), "sub-a", "client-1");
    let (models, store) = models_with_logins(provider, [old, other, fresh]);
    let interaction = Arc::new(NonInteractive);
    for _ in 0..3 {
        models
            .login(provider, AuthType::OAuth, interaction.clone(), None)
            .await
            .expect("commit sign-in");
    }
    let stored = store
        .read(provider, None)
        .await
        .expect("read credentials")
        .expect("entry");
    let slots = list_slots(Some(&PooledCredential::from(stored)));
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].name, "default");
    assert_eq!(slots[0].refresh.as_deref(), Some("refresh-fresh"));
    assert_eq!(slots[1].name, "login-2");
    assert_eq!(slots[1].refresh.as_deref(), Some("refresh-other"));
}

#[test]
fn one_signed_out_account_is_restored_without_touching_sibling() {
    let mut work = slot("work", &signed_out(alice("work", 1.0)));
    work.display_name = Some("Work".into());
    let home = bob("home", 1.0);
    let sibling = slot("home", &home);
    let fresh = alice("fresh", 9.0);
    let mut allocated = None;

    let next = relogin(
        pooled(home, vec![work, sibling.clone()]),
        fresh.clone(),
        Some("work"),
        |name| allocated = Some(name.to_owned()),
    )
    .expect("restore signed-out work slot");

    let mut restored = slot("work", &fresh);
    restored.display_name = Some("Work".into());
    assert_eq!(allocated.as_deref(), Some("work"));
    assert_eq!(list_slots(Some(&next)), [restored, sibling]);
    assert_eq!(next.refresh.as_deref(), Some("refresh-home"));
}

#[test]
fn whole_signed_out_credential_projects_restored_account() {
    let saved = signed_out(alice("old", 1.0));
    let work = slot("default", &saved);
    let fresh = alice("fresh", 9.0);

    let next = relogin(
        pooled(saved, vec![work]),
        fresh.clone(),
        Some("default"),
        |_| {},
    )
    .expect("restore signed-out credential");

    assert_eq!(list_slots(Some(&next)), [slot("default", &fresh)]);
    assert_eq!(next.refresh.as_deref(), Some("refresh-fresh"));
    assert_eq!(next.expires, Some(9.0));
    assert!(!next.extra.contains_key("signedOut"));
}

#[test]
fn legacy_reauthorization_refuses_another_persons_token() {
    let saved = alice("work", 1.0);
    let work = slot("work", &saved);

    let error = relogin(
        pooled(saved, vec![work]),
        bob("other", 9.0),
        Some("work"),
        |_| {
            panic!("a refused login must not allocate a slot");
        },
    )
    .expect_err("another person cannot replace the chosen slot");

    assert_account_mismatch(&error, "work");
}

#[test]
fn signed_out_legacy_reauthorization_refuses_other_workspace_and_restores_same_workspace() {
    let saved = signed_out(alice("work", 1.0));
    let work = slot("work", &saved);
    let stored = pooled(saved, vec![work]);
    let different_workspace = alice("x", 9.0).with_extra("accountId", json!("ws-2"));

    let error = relogin(stored.clone(), different_workspace, Some("work"), |_| {})
        .expect_err("different workspace must be refused");
    let next = relogin(stored, alice("fresh", 9.0), Some("work"), |_| {})
        .expect("same workspace should be restored");

    assert_account_mismatch(&error, "work");
    assert_eq!(slot_names(&next), ["work"]);
    assert_eq!(next.refresh.as_deref(), Some("refresh-fresh"));
}

#[test]
fn siwc_reauthorization_refuses_different_subject_or_route() {
    let saved = siwc(alice("work", 1.0), "sub-a", "client-1");
    let work = slot("work", &saved);
    let stored = pooled(saved, vec![work]);
    let different_subject = siwc(alice("x", 9.0), "sub-b", "client-1");

    for fresh in [different_subject, alice("x", 9.0)] {
        let error = relogin(stored.clone(), fresh, Some("work"), |_| {})
            .expect_err("different subject or route must be refused");
        assert_account_mismatch(&error, "work");
    }
}

#[tokio::test]
async fn models_login_surfaces_mismatch_and_leaves_stored_account_untouched() {
    let id = "chatgpt-reauth-mismatch";
    let (models, store) = models_with_logins(id, [bob("other", 9.0)]);
    let saved = alice("work", 1.0);
    let work = slot("work", &saved);
    let stored = pooled(saved.clone(), vec![work]).to_stored_credential();
    let seed = stored.clone();
    store
        .modify(
            id,
            Box::new(move |_| Box::pin(async move { Ok(Some(seed)) })),
            None,
        )
        .await
        .expect("seed work slot");

    let error = models
        .login(
            id,
            AuthType::OAuth,
            Arc::new(NonInteractive),
            Some(ModelsLoginOptions {
                oauth: Some(LoginOptions {
                    reauthorize: Some(saved),
                    reauthorize_slot: Some("work".into()),
                    ..LoginOptions::default()
                }),
                ..ModelsLoginOptions::default()
            }),
        )
        .await
        .expect_err("mismatch must reach the Models.login caller");

    assert_eq!(error.code, ModelsErrorCode::Auth);
    assert_account_mismatch(&error.message, "work");
    assert_eq!(
        store.read(id, None).await.expect("store read"),
        Some(stored)
    );
}

#[test]
fn named_oauth_slot_resolution_projects_accounts_only_json() {
    let stored: PooledCredential = serde_json::from_value(json!({
        "type": "oauth",
        "pinned": "siwc-work",
        "order": ["siwc-work", "legacy-home"],
        "accounts": [
            {
                "name": "siwc-work",
                "source": "login",
                "displayName": "Work",
                "access": "synthetic-siwc-access",
                "refresh": "synthetic-siwc-refresh",
                "expires": 25,
                "route": "siwc",
                "issuer": "https://auth.openai.com",
                "subject": "sub-work",
                "clientId": "client-work",
                "email": "work@example.test",
                "idToken": "synthetic-siwc-id",
                "scopes": ["openid", "chatgpt.tokens.use.direct"],
                "planUsageEnabled": false,
                "signedOut": false,
                "loginId": "siwc-login-id",
                "accountId": "ws-work",
                "env": {"REGION": "siwc-region"}
            },
            {
                "name": "legacy-home",
                "source": "login",
                "access": "synthetic-legacy-access",
                "refresh": "synthetic-legacy-refresh",
                "expires": 50,
                "loginId": "legacy-login-id",
                "accountId": "ws-home",
                "env": {"REGION": "legacy-region"}
            }
        ]
    }))
    .expect("accounts-only OAuth storage");
    assert!(stored.to_flat_credential().is_none());

    let legacy = project_slot(Some(&stored), "legacy-home");
    let siwc = project_slot(Some(&stored), "siwc-work");

    assert_eq!(
        legacy,
        Some(Credential::OAuth(
            OAuthCredential::new("synthetic-legacy-access", "synthetic-legacy-refresh", 50.0)
                .with_extra("loginId", json!("legacy-login-id"))
                .with_extra("accountId", json!("ws-home"))
                .with_extra("env", json!({"REGION": "legacy-region"}))
        ))
    );
    let expected_siwc = siwc_placement_registration(
        OAuthCredential::new("synthetic-siwc-access", "synthetic-siwc-refresh", 25.0),
        "sub-work",
        "client-work",
        "work@example.test",
        "synthetic-siwc-id",
    )
    .with_extra("planUsageEnabled", json!(false))
    .with_extra("signedOut", json!(false))
    .with_extra("loginId", json!("siwc-login-id"))
    .with_extra("accountId", json!("ws-work"))
    .with_extra("env", json!({"REGION": "siwc-region"}));
    assert_eq!(siwc, Some(Credential::OAuth(expected_siwc)));
    assert_eq!(project_slot(Some(&stored), "unknown"), None);
}

#[tokio::test]
async fn models_resolves_accounts_only_oauth_json_through_the_credential_store() {
    let credential: Credential = serde_json::from_value(json!({
        "type": "oauth",
        "expires": 4_102_444_800_000_u64,
        "accountId": "synthetic-workspace",
        "accounts": [
            {
                "name": "work",
                "source": "login",
                "access": "synthetic-work-access",
                "refresh": "synthetic-work-refresh",
                "expires": 4_102_444_800_000_u64,
                "route": "siwc",
                "issuer": "https://auth.openai.com",
                "subject": "synthetic-subject",
                "clientId": "synthetic-issued-client",
                "email": "work@example.test",
                "idToken": "synthetic-id-token",
                "scopes": ["openid", "chatgpt.tokens.use.direct"],
                "planUsageEnabled": true
            },
            {
                "name": "home",
                "source": "login",
                "access": "synthetic-home-access",
                "refresh": "synthetic-home-refresh",
                "expires": 4_102_444_800_000_u64
            }
        ]
    }))
    .expect("deserialize stored pooled credential");
    let store = Arc::new(InMemoryCredentialStore::new());
    let provider_id = "chatgpt-subscription";
    store
        .modify(
            provider_id,
            Box::new(|_| Box::pin(async { Ok(Some(credential)) })),
            None,
        )
        .await
        .expect("store credential");
    let models = create_models(Some(CreateModelsOptions {
        auth: Some(Arc::new(ProviderModelsAuth::new(
            store,
            Arc::new(default_provider_auth_context()),
        ))),
        ..Default::default()
    }));
    models.set_provider(chatgpt_subscription_provider().expect("shipped provider"));
    let selected = models
        .get_auth(
            provider_id,
            &AuthResolutionOverrides {
                slot_name: Some("work".into()),
                ..Default::default()
            },
        )
        .await
        .expect("auth resolution")
        .expect("selected account");
    assert_eq!(
        selected.auth.api_key.as_deref(),
        Some("synthetic-work-access")
    );
    assert_eq!(
        selected.auth.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
}

fn siwc_placement_registration(
    credential: OAuthCredential,
    subject: &str,
    client: &str,
    email: &str,
    id_token: &str,
) -> OAuthCredential {
    siwc(credential, subject, client)
        .with_extra("email", json!(email))
        .with_extra("idToken", json!(id_token))
        .with_extra("scopes", json!(["openid", "chatgpt.tokens.use.direct"]))
        .with_extra("planUsageEnabled", json!(true))
}

#[test]
fn preserves_each_siwc_registration_through_promotion_projection_refresh_and_removal() {
    let credential = |account: &str| {
        OAuthCredential::new(
            jwt(&json!({"chatgpt_account_id": account}), account),
            format!("synthetic-refresh-{account}"),
            4_102_444_800_000.0,
        )
    };
    let registration = |account: &str| {
        siwc_placement_registration(
            credential(account),
            account,
            &format!("oaiapp_{account}"),
            "same@example.test",
            &format!("synthetic-id-{account}"),
        )
    };
    let first = registration("first");
    let second = registration("second");
    let legacy = credential("legacy");

    let promoted = append_login_slot(Some(flat(first.clone())), flat(second.clone()), |_, _| {})
        .expect("promote second SIWC registration");
    let pool = append_login_slot(Some(promoted), flat(legacy.clone()), |_, _| {})
        .expect("append legacy account");
    assert_eq!(slot_names(&pool), ["default", "login-2", "login-3"]);
    assert_eq!(
        project_slot(Some(&pool), "login-2"),
        Some(Credential::OAuth(second.clone()))
    );
    assert_eq!(
        project_slot(Some(&pool), "login-3"),
        Some(Credential::OAuth(legacy.clone()))
    );

    let mut rotated = second
        .with_extra("scopes", json!(["openid"]))
        .with_extra("planUsageEnabled", json!(false));
    rotated.access = "synthetic-rotated".into();
    rotated.refresh = "synthetic-rotated-refresh".into();
    let rotated = Credential::OAuth(rotated);
    let next = merge_refreshed_slot(&pool, "login-2", &rotated);
    assert_eq!(project_slot(Some(&next), "login-2"), Some(rotated));
    assert_eq!(
        project_slot(Some(&next), "default"),
        Some(Credential::OAuth(first))
    );
    assert_eq!(
        project_slot(Some(&next), "login-3"),
        Some(Credential::OAuth(legacy.clone()))
    );

    let survivor = remove_slot(remove_slot(Some(next), "default"), "login-2")
        .expect("legacy account survives both removals");
    assert_eq!(slot_names(&survivor), ["login-3"]);
    assert_eq!(
        project_slot(Some(&survivor), "login-3"),
        Some(Credential::OAuth(legacy))
    );
    assert!(!survivor.extra.contains_key("route"));
    assert!(!survivor.extra.contains_key("clientId"));
}
