// Port of senpi test/mcp/4194-oauth-{metadata-issuer,client-issuer,root-fallback}.test.ts against a local fixture.
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
};
use maho_ext_mcp::auth::{
    oauth::{
        OAuthRequestError, OAuthServerInfo, begin_authorization, client_credentials_grant,
        complete_authorization, reset_discovery_cache,
    },
    oauth_errors::OAuthFailureKind,
    oauth_issuer::{
        canonical_authorization_server_issuer, credentials_match_issuer,
        validated_authorization_server_issuer,
    },
    oauth_provider::McpOAuthProvider,
    oauth_refresh::McpRefreshManager,
    token_store::{McpStoredAuth, McpTokenStore},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Idp {
    issuer: String,
    metadata_issuer: String,
    prm: bool,
    reject_registration: bool,
    registrations: usize,
    requests: Vec<(String, String)>,
}
type Shared = Arc<Mutex<Idp>>;

async fn handle(State(idp): State<Shared>, uri: Uri, body: String) -> Response {
    let path = uri.path().to_owned();
    let mut idp = idp.lock().unwrap();
    if path.starts_with("/.well-known/oauth-protected-resource") {
        return if idp.prm {
            Json(json!({"authorization_servers":[idp.issuer]})).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        };
    }
    if path.starts_with("/.well-known/") {
        let issuer = idp.issuer.clone();
        return Json(json!({"issuer":idp.metadata_issuer,"authorization_endpoint":format!("{issuer}/authorize"),"token_endpoint":format!("{issuer}/token"),
            "registration_endpoint":format!("{issuer}/register"),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"]}))
        .into_response();
    }
    idp.requests.push((path.clone(), body.clone()));
    if path.ends_with("/register") {
        idp.registrations += 1;
        if idp.reject_registration {
            return StatusCode::BAD_REQUEST.into_response();
        }
        return Json(
            json!({"client_id":format!("registered-{}",idp.registrations),"redirect_uris":[]}),
        )
        .into_response();
    }
    Json(json!({"access_token":"new-access","refresh_token":"new-refresh","token_type":"Bearer"}))
        .into_response()
}

struct Fixture {
    idp: Shared,
    base: String,
    issuer: String,
    resource: String,
    root: tempfile::TempDir,
    client: reqwest::Client,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    fn store(&self) -> McpTokenStore {
        McpTokenStore::new(self.root.path(), "issuer", &self.resource)
    }
    fn provider(&self, client_id: Option<&str>) -> McpOAuthProvider {
        let mut provider = McpOAuthProvider::new(self.store());
        provider.require_https = false;
        provider.redirect_url = Some("http://127.0.0.1:8123/callback".into());
        provider.client_id = client_id.map(str::to_owned);
        provider
    }
    fn requests(&self) -> Vec<(String, String)> {
        self.idp.lock().unwrap().requests.clone()
    }
    fn set(&self, change: impl FnOnce(&mut Idp)) {
        change(&mut self.idp.lock().unwrap());
    }
}
async fn fixture() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let issuer = format!("{base}/tenant");
    let idp: Shared = Arc::new(Mutex::new(Idp {
        issuer: issuer.clone(),
        metadata_issuer: issuer.clone(),
        prm: true,
        ..Default::default()
    }));
    let app = Router::new().fallback(handle).with_state(idp.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // The process-wide discovery cache is keyed by resource; every fixture uses a fresh port.
    Fixture {
        idp,
        resource: format!("{base}/mcp"),
        base,
        issuer,
        root: tempfile::tempdir().unwrap(),
        client: reqwest::Client::new(),
        task,
    }
}
fn needs_auth<T>(result: Result<T, OAuthRequestError>) -> String {
    match result {
        Err(OAuthRequestError::Flow(error)) if error.oauth_kind == OAuthFailureKind::NeedsAuth => {
            error.to_string()
        }
        Err(other) => panic!("expected needs_auth, got {other:?}"),
        Ok(_) => panic!("expected needs_auth, got success"),
    }
}

#[test]
fn canonical_issuer_strips_only_an_origin_trailing_slash() {
    assert_eq!(
        canonical_authorization_server_issuer("https://a.example/"),
        "https://a.example"
    );
    assert_eq!(
        canonical_authorization_server_issuer("https://a.example/tenant/"),
        "https://a.example/tenant/"
    );
    assert_eq!(
        canonical_authorization_server_issuer("https://a.example/?q"),
        "https://a.example/?q"
    );
}

#[test]
fn metadata_issuer_is_compared_verbatim_and_only_the_root_fallback_is_normalized() {
    let info = |url: &str, issuer: &str, prm: bool| OAuthServerInfo {
        authorization_server_url: url.into(),
        authorization_server_metadata: json!({"issuer": issuer}),
        resource_metadata: if prm {
            json!({"authorization_servers":[url]})
        } else {
            Value::Null
        },
    };
    assert_eq!(
        validated_authorization_server_issuer(
            &info("https://r.example/", "https://r.example", false),
            "s"
        )
        .unwrap(),
        "https://r.example"
    );
    assert!(
        validated_authorization_server_issuer(
            &info("https://r.example/", "https://r.example", true),
            "s"
        )
        .is_err()
    );
    assert!(
        validated_authorization_server_issuer(
            &info("https://r.example", "https://r.example/", false),
            "s"
        )
        .is_err()
    );
    assert!(
        validated_authorization_server_issuer(
            &info(
                "https://a.example/tenant",
                "https://a.example/tenant/",
                true
            ),
            "s"
        )
        .is_err()
    );
    assert_eq!(
        validated_authorization_server_issuer(
            &info("https://a.example/tenant", "https://a.example/tenant", true),
            "s"
        )
        .unwrap(),
        "https://a.example/tenant"
    );
}

#[test]
fn every_credential_stamp_must_name_the_issuer() {
    let record = |grant: Option<&str>, client: Option<&str>| McpStoredAuth {
        issuer: grant.map(str::to_owned),
        client_info: Some(client.map_or_else(
            || json!({"client_id":"c"}),
            |issuer| json!({"client_id":"c","issuer":issuer}),
        )),
        ..Default::default()
    };
    assert!(credentials_match_issuer(None, "https://a"));
    assert!(credentials_match_issuer(
        Some(&record(None, None)),
        "https://a"
    ));
    assert!(credentials_match_issuer(
        Some(&record(Some("https://a/"), None)),
        "https://a"
    ));
    assert!(!credentials_match_issuer(
        Some(&record(Some("https://a"), Some("https://b"))),
        "https://a"
    ));
    assert!(!credentials_match_issuer(
        Some(&record(None, Some("https://a/tenant/"))),
        "https://a/tenant"
    ));
}

#[tokio::test]
async fn rejects_a_metadata_document_naming_another_issuer_before_any_request() {
    for wrong in ["https://other.example.com", "/tenant/"] {
        let fixture = fixture().await;
        let wrong = if wrong.starts_with('/') {
            format!("{}{wrong}", fixture.base)
        } else {
            wrong.to_owned()
        };
        fixture.set(|idp| idp.metadata_issuer = wrong);
        needs_auth(begin_authorization(&mut fixture.provider(None), &fixture.client).await);
        assert!(fixture.requests().is_empty());
    }
}

#[tokio::test]
async fn validates_rediscovered_metadata_before_a_refresh_sends_credentials() {
    let fixture = fixture().await;
    fixture
        .store()
        .write(McpStoredAuth {
            issuer: Some(fixture.issuer.clone()),
            access_token: Some("old-access".into()),
            refresh_token: Some("old-refresh".into()),
            expires_at: Some(0.0),
            ..Default::default()
        })
        .unwrap();
    fixture.set(|idp| idp.metadata_issuer = "https://other.example.com".into());
    let manager = McpRefreshManager::new(
        Arc::new(fixture.provider(Some("configured"))),
        fixture.client.clone(),
    );
    needs_auth(manager.refresh().await);
    assert!(fixture.requests().is_empty());
}

#[tokio::test]
async fn discovers_a_path_resource_without_prm_using_the_root_metadata_origin() {
    let fixture = fixture().await;
    fixture.set(|idp| {
        idp.prm = false;
        idp.issuer = fixture.base.clone();
        idp.metadata_issuer = fixture.base.clone();
    });
    assert!(
        !begin_authorization(&mut fixture.provider(None), &fixture.client)
            .await
            .unwrap()
            .authorized
    );
    fixture.set(|idp| idp.metadata_issuer = format!("{}/", fixture.base));
    reset_discovery_cache();
    needs_auth(begin_authorization(&mut fixture.provider(None), &fixture.client).await);
}

#[tokio::test]
async fn checks_a_pasted_error_response_issuer_before_exposing_its_error() {
    let fixture = fixture().await;
    let mut provider = fixture.provider(None);
    let begin = begin_authorization(&mut provider, &fixture.client)
        .await
        .unwrap();
    let state = begin
        .authorization_url
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let mut input = url::Url::parse("http://127.0.0.1:8123/callback").unwrap();
    input
        .query_pairs_mut()
        .append_pair("state", &state)
        .append_pair("iss", "https://other.example.com")
        .append_pair("error", "untrusted-server-error");
    let message =
        needs_auth(complete_authorization(&mut provider, input.as_str(), &fixture.client).await);
    assert!(!message.contains("untrusted-server-error"), "{message}");
}

#[tokio::test]
async fn rejects_a_callback_when_its_credential_stamps_conflict() {
    let fixture = fixture().await;
    let mut provider = fixture.provider(None);
    let begin = begin_authorization(&mut provider, &fixture.client)
        .await
        .unwrap();
    let state = begin
        .authorization_url
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    fixture
        .store()
        .update(|current| {
            current.map(|mut record| {
                record.issuer = Some("https://other.example.com".into());
                record
            })
        })
        .unwrap();
    let before = fixture.requests().len();
    let redirect = format!(
        "http://127.0.0.1:8123/callback?code=code&state={state}&iss=https://other.example.com"
    );
    needs_auth(complete_authorization(&mut provider, &redirect, &fixture.client).await);
    assert_eq!(fixture.requests().len(), before);
}

#[tokio::test]
async fn never_sends_an_old_registration_to_a_changed_issuer() {
    let other = fixture().await;
    let other_issuer = other.issuer.clone();
    for changed in ["/other", other_issuer.as_str()] {
        let fixture = fixture().await;
        let changed = if changed.starts_with('/') {
            format!("{}{changed}", fixture.base)
        } else {
            changed.to_owned()
        };
        let issuer = fixture.issuer.clone();
        fixture
            .store()
            .write(McpStoredAuth {
                issuer: Some(issuer.clone()),
                access_token: Some("old-access".into()),
                refresh_token: Some("old-refresh".into()),
                client_info: Some(json!({"client_id":"old-client","client_secret":"old-secret","redirect_uris":[],"issuer":issuer})),
                ..Default::default()
            })
            .unwrap();
        fixture.set(|idp| {
            idp.issuer = changed.clone();
            idp.metadata_issuer = changed.clone();
        });
        needs_auth(client_credentials_grant(&fixture.provider(None), &fixture.client).await);
        assert!(fixture.requests().is_empty());
        assert!(fixture.store().read().unwrap().is_none());
    }
}

#[tokio::test]
async fn rejects_a_foreign_registration_even_when_the_refresh_token_issuer_matches() {
    let fixture = fixture().await;
    let issuer = fixture.issuer.clone();
    fixture
        .store()
        .write(McpStoredAuth {
            issuer: Some(issuer.clone()),
            access_token: Some("old-access".into()),
            refresh_token: Some("old-refresh".into()),
            expires_at: Some(0.0),
            client_info: Some(json!({"client_id":"foreign","client_secret":"old-secret","redirect_uris":[],"issuer":format!("{issuer}/")})),
            ..Default::default()
        })
        .unwrap();
    needs_auth(
        McpRefreshManager::new(Arc::new(fixture.provider(None)), fixture.client.clone())
            .refresh()
            .await,
    );
    assert!(fixture.requests().is_empty());
    assert!(fixture.store().read().unwrap().is_none());
}

#[tokio::test]
async fn stamps_tokens_and_registrations_and_reuses_the_registration_after_restart() {
    let fixture = fixture().await;
    let mut provider = fixture.provider(None);
    let begin = begin_authorization(&mut provider, &fixture.client)
        .await
        .unwrap();
    let state = begin
        .authorization_url
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    complete_authorization(
        &mut provider,
        &format!("http://127.0.0.1:8123/callback?code=code&state={state}"),
        &fixture.client,
    )
    .await
    .unwrap();
    let record = fixture.store().read().unwrap().unwrap();
    assert_eq!(record.issuer.as_deref(), Some(fixture.issuer.as_str()));
    let client_info = record.client_info.unwrap();
    assert_eq!(client_info["issuer"], fixture.issuer.as_str());
    assert_eq!(client_info["client_id"], "registered-1");
    let mut restarted = fixture.provider(None);
    begin_authorization(&mut restarted, &fixture.client)
        .await
        .unwrap();
    assert_eq!(fixture.idp.lock().unwrap().registrations, 1);
    let registration: Value = serde_json::from_str(&fixture.requests()[0].1).unwrap();
    assert_eq!(registration["application_type"], "native");
}

#[tokio::test]
async fn deletes_a_mismatched_registration_before_a_failed_re_registration() {
    let fixture = fixture().await;
    fixture
        .store()
        .write(McpStoredAuth {
            issuer: Some("https://old.example.com".into()),
            access_token: Some("old-access".into()),
            client_info: Some(json!({"issuer":"https://old.example.com","client_id":"old-client","redirect_uris":[]})),
            ..Default::default()
        })
        .unwrap();
    fixture.set(|idp| idp.reject_registration = true);
    assert!(
        begin_authorization(&mut fixture.provider(None), &fixture.client)
            .await
            .is_err()
    );
    let record = fixture.store().read().unwrap();
    assert!(
        record
            .as_ref()
            .is_none_or(|record| record.client_info.is_none() && record.access_token.is_none())
    );
    assert_eq!(
        fixture
            .requests()
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>(),
        ["/tenant/register"]
    );
}

#[test]
fn does_not_declare_a_redirect_application_type_without_redirects() {
    let root = tempfile::tempdir().unwrap();
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(
        root.path(),
        "issuer",
        "https://r.example/mcp",
    ));
    assert!(provider.client_metadata().get("application_type").is_none());
    provider.redirect_url = Some("https://app.example/callback".into());
    assert_eq!(provider.client_metadata()["application_type"], "web");
}

// Port of senpi test/mcp/2940-oauth-issuer-binding.test.ts (GHSA-6qxp-vccf-f47h): stored credentials are bound to
// the authorization server that issued them. A refresh token is never posted to another authorization server, and a
// mismatch is a continuity break: the credentials are discarded and the user signs in again.
const OTHER_AS: &str = "https://other-authorization-server.invalid";

fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}
fn token_hits(fixture: &Fixture) -> usize {
    fixture
        .requests()
        .iter()
        .filter(|(path, _)| path.ends_with("/token"))
        .count()
}
fn refresh_requests(fixture: &Fixture) -> usize {
    fixture
        .requests()
        .iter()
        .filter(|(_, body)| body.contains("grant_type=refresh_token"))
        .count()
}
fn refresh_manager(fixture: &Fixture) -> McpRefreshManager {
    McpRefreshManager::new(
        Arc::new(fixture.provider(Some("static-client"))),
        fixture.client.clone(),
    )
}
async fn sign_in(fixture: &Fixture) {
    let mut provider = fixture.provider(Some("static-client"));
    let begin = begin_authorization(&mut provider, &fixture.client)
        .await
        .unwrap();
    let state = begin
        .authorization_url
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    complete_authorization(
        &mut provider,
        &format!("http://127.0.0.1:8123/callback?code=code&state={state}"),
        &fixture.client,
    )
    .await
    .unwrap();
}
fn expire_soon(fixture: &Fixture, change: impl FnOnce(&mut McpStoredAuth)) {
    fixture
        .store()
        .update(|current| {
            let mut record = current.unwrap();
            record.expires_at = Some(now_ms() + 60_000.0);
            change(&mut record);
            Some(record)
        })
        .unwrap();
}
fn unattributed_grant(fixture: &Fixture) -> McpStoredAuth {
    McpStoredAuth {
        access_token: Some("AT_older".into()),
        refresh_token: Some("RT_older".into()),
        expires_at: Some(now_ms() + 60_000.0),
        resource: Some(fixture.resource.clone()),
        ..Default::default()
    }
}
fn terminal_needs_auth<T>(result: Result<T, OAuthRequestError>) {
    match result {
        Err(OAuthRequestError::Flow(error)) => {
            assert_eq!(error.oauth_kind, OAuthFailureKind::NeedsAuth);
            assert!(error.terminal);
        }
        Err(other) => panic!("expected needs_auth, got {other:?}"),
        Ok(_) => panic!("expected needs_auth, got success"),
    }
}

#[tokio::test]
async fn binding_a_sign_in_records_the_issuer_and_hands_the_refresh_token_out() {
    let fixture = fixture().await;
    sign_in(&fixture).await;
    let record = fixture.store().read().unwrap().unwrap();
    assert_eq!(record.issuer.as_deref(), Some(fixture.issuer.as_str()));
    let tokens = fixture.provider(None).tokens(now_ms()).unwrap().unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
}

#[tokio::test]
async fn binding_tokens_from_another_issuer_are_discarded_without_sending_the_refresh_token() {
    let fixture = fixture().await;
    sign_in(&fixture).await;
    expire_soon(&fixture, |record| record.issuer = Some(OTHER_AS.into()));
    let before = token_hits(&fixture);
    terminal_needs_auth(refresh_manager(&fixture).ensure_fresh().await);
    assert_eq!(token_hits(&fixture), before);
    assert!(fixture.store().read().unwrap().is_none());
}

#[tokio::test]
async fn binding_a_refresh_at_the_issuing_server_keeps_the_issuer_stamp() {
    let fixture = fixture().await;
    sign_in(&fixture).await;
    expire_soon(&fixture, |_| {});
    refresh_manager(&fixture).ensure_fresh().await.unwrap();
    let record = fixture.store().read().unwrap().unwrap();
    assert_eq!(record.issuer.as_deref(), Some(fixture.issuer.as_str()));
}

#[tokio::test]
async fn binding_a_legacy_discovery_record_attributes_the_grant_and_refreshes_there() {
    let fixture = fixture().await;
    sign_in(&fixture).await;
    let issuer = fixture.issuer.clone();
    expire_soon(&fixture, |record| {
        record.issuer = None;
        record.extra.insert(
            "discoveryState".into(),
            json!({"authorizationServerUrl": issuer}),
        );
    });
    let before = token_hits(&fixture);
    refresh_manager(&fixture).ensure_fresh().await.unwrap();
    assert_eq!(token_hits(&fixture), before + 1);
    let record = fixture.store().read().unwrap().unwrap();
    assert_eq!(record.issuer.as_deref(), Some(fixture.issuer.as_str()));
}

#[tokio::test]
async fn binding_an_unattributed_legacy_grant_sends_nothing_and_requires_sign_in() {
    let fixture = fixture().await;
    fixture.store().write(unattributed_grant(&fixture)).unwrap();
    let tokens = fixture.provider(None).tokens(now_ms()).unwrap().unwrap();
    assert!(tokens.refresh_token.is_none());
    terminal_needs_auth(refresh_manager(&fixture).ensure_fresh().await);
    assert_eq!(token_hits(&fixture), 0);
    assert!(fixture.store().read().unwrap().is_none());
}

#[tokio::test]
async fn binding_discovery_never_sends_an_unattributed_refresh_token() {
    for run in ["begin_authorization", "client_credentials_grant"] {
        let fixture = fixture().await;
        fixture.store().write(unattributed_grant(&fixture)).unwrap();
        let provider = fixture.provider(Some("static-client"));
        if run == "begin_authorization" {
            let mut provider = provider;
            begin_authorization(&mut provider, &fixture.client)
                .await
                .unwrap();
        } else {
            client_credentials_grant(&provider, &fixture.client)
                .await
                .unwrap();
        }
        assert_eq!((run, refresh_requests(&fixture)), (run, 0));
        // A client_credentials grant may legitimately mint a new refresh token; the old one is gone either way.
        let record = fixture.store().read().unwrap().unwrap();
        assert_ne!(
            record.refresh_token.as_deref(),
            Some("RT_older"),
            "{run} kept the unattributed refresh token"
        );
    }
}

// Upstream asserts the stamp survives the SDK's rediscovery because its persisted discovery record is reused; mhc
// has no persisted discovery, so the grant is attributed from the legacy record and dropped by the mismatch instead.
// Either way nothing is sent and the grant is never re-attributed to the server discovered now.
#[tokio::test]
async fn binding_a_grant_whose_sign_in_discovered_another_server_stays_bound_to_it() {
    let fixture = fixture().await;
    let mut grant = unattributed_grant(&fixture);
    grant.extra.insert(
        "discoveryState".into(),
        json!({"authorizationServerUrl": OTHER_AS}),
    );
    fixture.store().write(grant).unwrap();
    let record = fixture.store().read().unwrap().unwrap();
    assert_eq!(record.issuer.as_deref(), Some(OTHER_AS));

    begin_authorization(
        &mut fixture.provider(Some("static-client")),
        &fixture.client,
    )
    .await
    .unwrap();
    assert_eq!(refresh_requests(&fixture), 0);
    assert!(
        fixture
            .store()
            .read()
            .unwrap()
            .is_none_or(|record| record.refresh_token.is_none()
                && record.issuer.as_deref() != Some(fixture.issuer.as_str()))
    );
}
