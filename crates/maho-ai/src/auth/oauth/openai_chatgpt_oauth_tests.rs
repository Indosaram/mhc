//! Sign-in, consent, and refresh cases from packages/ai/test/openai-chatgpt-oauth.test.ts.

use super::*;
use crate::auth::oauth::openai_id_token::test_support::{claims, discovery, jwks, signed_token};
use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
use crate::auth::types::{AuthInteraction, PlanUsageConsent};
use std::collections::HashMap;
use std::sync::Mutex;

const DEVICE_ID: &str = "e61bbe28-07ef-466d-8e5d-a344f94ab305";
const ISSUED_CLIENT: &str = "oaiapp_test";

#[derive(Clone, Copy)]
enum CallbackKind {
    Manual,
    Loopback,
}

struct FakeLogin {
    urls: Mutex<Vec<Url>>,
    ready: tokio::sync::Notify,
    kind: CallbackKind,
    callback_client: Option<&'static str>,
    decline: bool,
    manual_path: Option<&'static str>,
}

impl FakeLogin {
    fn authorization(&self) -> Url {
        self.urls.lock().unwrap().last().unwrap().clone()
    }

    fn callback(&self, foreign: bool) -> Url {
        let url = self.authorization();
        let fields = url.query_pairs().collect::<HashMap<_, _>>();
        let mut callback = Url::parse(&fields["redirect_uri"]).unwrap();
        let mut query = callback.query_pairs_mut();
        query.append_pair("state", if foreign { "foreign" } else { &fields["state"] });
        if self.decline {
            query.append_pair("error", "access_denied");
        } else {
            query.append_pair("code", "synthetic-code");
            if let Some(client) = self.callback_client {
                query.append_pair("client_id", client);
            }
        }
        drop(query);
        callback
    }
}

#[async_trait]
impl AuthInteraction for FakeLogin {
    fn signal(&self) -> Option<AbortSignal> {
        None
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        let AuthPromptKind::ManualCode { placeholder, .. } = prompt.kind else {
            anyhow::bail!("expected the manual callback prompt");
        };
        match self.kind {
            CallbackKind::Manual => {
                let mut callback = self.callback(false);
                assert!(callback.as_str().starts_with(&placeholder.unwrap()));
                if let Some(path) = self.manual_path {
                    callback.set_path(path);
                }
                Ok(callback.into())
            }
            CallbackKind::Loopback => std::future::pending().await,
        }
    }

    fn notify(&self, event: AuthEvent) {
        if let AuthEvent::AuthUrl { url, .. } = event {
            self.urls.lock().unwrap().push(Url::parse(&url).unwrap());
            self.ready.notify_one();
        }
    }
}

struct FlowTransport {
    login: Arc<FakeLogin>,
    scripted: ScriptedTransport,
    subject: &'static str,
    scope: &'static str,
}

#[async_trait]
impl OAuthTransport for FlowTransport {
    fn now_ms(&self) -> f64 {
        1000.0
    }

    async fn execute(
        &self,
        request: HttpRequest,
        signal: &AbortSignal,
    ) -> anyhow::Result<HttpResponse> {
        if request.url == TOKEN_URL {
            let url = self.login.authorization();
            let query = url.query_pairs().collect::<HashMap<_, _>>();
            let form = url::form_urlencoded::parse(request.body.as_deref().unwrap().as_bytes())
                .collect::<HashMap<_, _>>();
            assert_eq!(form["grant_type"], "authorization_code");
            assert_eq!(form["redirect_uri"], query["redirect_uri"]);
            assert_eq!(form["code"], "synthetic-code");
            assert_eq!(form["resource"], RESOURCE);
            assert!(!form["code_verifier"].is_empty());
            let mut identity = claims(&form["client_id"], &query["nonce"]);
            identity["sub"] = json!(self.subject);
            let mut response = tests::token();
            response["scope"] = json!(self.scope);
            response["id_token"] = json!(signed_token(
                identity,
                json!({"alg":"RS256","kid":"test-key"})
            ));
            self.scripted.push(
                Some(TOKEN_URL),
                ScriptedResponse::Json {
                    status: 200,
                    body: response,
                },
            );
        }
        self.scripted.execute(request, signal).await
    }
}

fn fixture(
    kind: CallbackKind,
    callback_client: Option<&'static str>,
    decline: bool,
    subject: &'static str,
    scope: &'static str,
    port: u16,
) -> (OpenAIChatGPTOAuth, Arc<FakeLogin>, Arc<FlowTransport>) {
    let login = Arc::new(FakeLogin {
        urls: Mutex::new(Vec::new()),
        ready: tokio::sync::Notify::new(),
        kind,
        callback_client,
        decline,
        manual_path: None,
    });
    let transport = Arc::new(FlowTransport {
        login: login.clone(),
        scripted: ScriptedTransport::new(vec![
            (Some("openid-configuration"), discovery()),
            (Some("/jwks"), jwks("test-key")),
        ]),
        subject,
        scope,
    });
    (
        OpenAIChatGPTOAuth::new(transport.clone()).with_callback_port(port),
        login,
        transport,
    )
}

fn interaction(login: Arc<FakeLogin>) -> ProviderAuthInteraction {
    ProviderAuthInteraction::new(AbortController::new().signal(), login).with_device_id(DEVICE_ID)
}

fn saved() -> OAuthCredential {
    OAuthCredential::new("saved-access", "saved-refresh", 0.0)
        .with_extra("clientId", json!(ISSUED_CLIENT))
        .with_extra("subject", json!("test-subject"))
        .with_extra("email", json!("saved@example.test"))
        .with_extra("idToken", json!("retained-id-token"))
        .with_extra("scopes", json!([DIRECT_TOKEN_SCOPE]))
}

async fn login_with_callback(
    flow: &OpenAIChatGPTOAuth,
    fake: &Arc<FakeLogin>,
    options: Option<LoginOptions>,
    foreign_decline_first: bool,
    path_check: Option<&str>,
) -> anyhow::Result<OAuthCredential> {
    let interaction = interaction(fake.clone());
    let login = flow.login_with_options(&interaction, options);
    if matches!(fake.kind, CallbackKind::Manual) {
        return login.await;
    }
    let browser = async {
        fake.ready.notified().await;
        let client = reqwest::Client::builder().no_proxy().build()?;
        if foreign_decline_first {
            let response = client.get(fake.callback(true)).send().await?;
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
            assert!(response.text().await?.contains("OAuth state mismatch"));
        }
        if let Some(path) = path_check {
            let mut wrong = fake.callback(false);
            wrong.set_path(path);
            let response = client.get(wrong).send().await?;
            assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        }
        client.get(fake.callback(false)).send().await?;
        Ok::<_, anyhow::Error>(())
    };
    let (result, browser) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(login, browser)
    })
    .await?;
    browser?;
    result
}

#[tokio::test]
async fn registers_user_owned_client_and_reuses_installation_host_id() {
    let (flow, fake, transport) = fixture(
        CallbackKind::Manual,
        Some(ISSUED_CLIENT),
        false,
        "test-subject",
        DIRECT_TOKEN_SCOPE,
        0,
    );
    for _ in 0..2 {
        let credential = login_with_callback(
            &flow,
            &fake,
            Some(LoginOptions {
                app_name: Some("test agent".into()),
                ..Default::default()
            }),
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(credential.get_extra_str("clientId"), Some(ISSUED_CLIENT));
        assert_eq!(credential.get_extra_str("subject"), Some("test-subject"));
        assert_eq!(
            credential.get_extra_str("issuer"),
            Some("https://auth.openai.com")
        );
        assert_eq!(credential.get_extra_str("email"), Some("test@example.test"));
        assert!(credential.get_extra_str("idToken").is_some());
        assert_eq!(
            credential.get_extra("scopes"),
            Some(&json!([DIRECT_TOKEN_SCOPE]))
        );
        assert_eq!(
            flow.to_auth(&credential).await.unwrap().api_key.as_deref(),
            Some("access")
        );
    }
    let urls = fake.urls.lock().unwrap();
    let first = urls[0].query_pairs().collect::<HashMap<_, _>>();
    let second = urls[1].query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(first["client_id"], DYNAMIC_CLIENT_ID);
    assert_eq!(first["agent_name_hint"], "test agent");
    assert_eq!(first["ext_agent_host_id"], format!("urn:uuid:{DEVICE_ID}"));
    assert_eq!(first["ext_agent_host_id"], second["ext_agent_host_id"]);
    assert_ne!(first["nonce"], second["nonce"]);
    assert_ne!(first["state"], second["state"]);
    assert_eq!(transport.scripted.requests().len(), 4);
}

#[tokio::test]
async fn reauthorizes_saved_client_with_or_without_callback_client() {
    for kind in [CallbackKind::Manual, CallbackKind::Loopback] {
        for callback_client in [None, Some(ISSUED_CLIENT)] {
            let (flow, fake, transport) = fixture(
                kind,
                callback_client,
                false,
                "test-subject",
                DIRECT_TOKEN_SCOPE,
                0,
            );
            let credential = login_with_callback(
                &flow,
                &fake,
                Some(LoginOptions {
                    reauthorize: Some(saved()),
                    ..Default::default()
                }),
                false,
                None,
            )
            .await
            .unwrap();
            let authorization = fake.authorization();
            let query = authorization.query_pairs().collect::<HashMap<_, _>>();
            assert_eq!(query["client_id"], ISSUED_CLIENT);
            assert_eq!(query["id_token_hint"], "retained-id-token");
            assert_eq!(query["login_hint"], "saved@example.test");
            assert!(!query.contains_key("agent_name_hint"));
            assert_eq!(credential.get_extra_str("subject"), Some("test-subject"));
            assert_eq!(credential.get_extra_str("clientId"), Some(ISSUED_CLIENT));
            assert_eq!(transport.scripted.requests().len(), 3);
        }
    }
}

#[tokio::test]
async fn reauthorizes_signed_out_registration_without_retained_hints() {
    let (flow, fake, _) = fixture(
        CallbackKind::Manual,
        None,
        false,
        "test-subject",
        DIRECT_TOKEN_SCOPE,
        0,
    );
    let mut credential = saved().with_extra("signedOut", json!(true));
    credential.extra.remove("idToken");
    credential.extra.remove("email");
    credential.access.clear();
    credential.refresh.clear();
    login_with_callback(
        &flow,
        &fake,
        Some(LoginOptions {
            reauthorize: Some(credential),
            ..Default::default()
        }),
        false,
        None,
    )
    .await
    .unwrap();
    let authorization = fake.authorization();
    let query = authorization.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(query["client_id"], ISSUED_CLIENT);
    assert!(!query.contains_key("id_token_hint"));
    assert!(!query.contains_key("login_hint"));
}

#[tokio::test]
async fn rejects_changed_client_and_changed_verified_subject_when_reauthorizing() {
    for kind in [CallbackKind::Manual, CallbackKind::Loopback] {
        let (flow, fake, transport) = fixture(
            kind,
            Some("other-client"),
            false,
            "test-subject",
            DIRECT_TOKEN_SCOPE,
            0,
        );
        let error = login_with_callback(
            &flow,
            &fake,
            Some(LoginOptions {
                reauthorize: Some(saved()),
                ..Default::default()
            }),
            false,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OpenAIReauthorizationError>(),
            Some(OpenAIReauthorizationError::ClientMismatch)
        ));
        assert!(transport.scripted.requests().is_empty());
    }
    let (flow, fake, transport) = fixture(
        CallbackKind::Manual,
        None,
        false,
        "another-subject",
        DIRECT_TOKEN_SCOPE,
        0,
    );
    let original = saved();
    let error = login_with_callback(
        &flow,
        &fake,
        Some(LoginOptions {
            reauthorize: Some(original.clone()),
            ..Default::default()
        }),
        false,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<OpenAIReauthorizationError>(),
        Some(OpenAIReauthorizationError::SubjectMismatch)
    ));
    assert_eq!(original.get_extra_str("subject"), Some("test-subject"));
    assert_eq!(transport.scripted.requests().len(), 3);
}

#[tokio::test]
async fn rejects_incomplete_saved_identity_or_missing_device_before_authorization() {
    let (flow, fake, transport) = fixture(
        CallbackKind::Manual,
        None,
        false,
        "test-subject",
        DIRECT_TOKEN_SCOPE,
        0,
    );
    for (field, value) in [
        ("clientId", Value::Null),
        ("clientId", json!(DYNAMIC_CLIENT_ID)),
        ("subject", Value::Null),
    ] {
        let error = flow
            .login_with_options(
                &interaction(fake.clone()),
                Some(LoginOptions {
                    reauthorize: Some(saved().with_extra(field, value)),
                    ..Default::default()
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OpenAIReauthorizationError>(),
            Some(OpenAIReauthorizationError::InvalidAccount)
        ));
    }
    let no_device = ProviderAuthInteraction::new(AbortController::new().signal(), fake.clone());
    assert!(
        flow.login(&no_device)
            .await
            .unwrap_err()
            .to_string()
            .contains("requires a device ID")
    );
    assert!(fake.urls.lock().unwrap().is_empty());
    assert!(transport.scripted.requests().is_empty());
}

#[tokio::test]
async fn binds_an_ephemeral_callback_when_preferred_port_is_occupied_and_rejects_wrong_path() {
    let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = held.local_addr().unwrap().port();
    let (flow, fake, _) = fixture(
        CallbackKind::Loopback,
        Some(ISSUED_CLIENT),
        false,
        "test-subject",
        DIRECT_TOKEN_SCOPE,
        port,
    );
    login_with_callback(&flow, &fake, None, false, Some("/callback"))
        .await
        .unwrap();
    assert_ne!(fake.callback(false).port(), Some(port));
}

#[tokio::test]
async fn validates_the_actual_fallback_uri_on_manual_input() {
    let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = held.local_addr().unwrap().port();
    for (path, succeeds) in [(CALLBACK_PATH, true), ("/callback", false)] {
        let settings = FakeLogin {
            urls: Mutex::new(Vec::new()),
            ready: tokio::sync::Notify::new(),
            kind: CallbackKind::Manual,
            callback_client: Some(ISSUED_CLIENT),
            decline: false,
            manual_path: Some(path),
        };
        let fake = Arc::new(settings);
        let transport = Arc::new(FlowTransport {
            login: fake.clone(),
            scripted: ScriptedTransport::new(vec![
                (Some("openid-configuration"), discovery()),
                (Some("/jwks"), jwks("test-key")),
            ]),
            subject: "test-subject",
            scope: DIRECT_TOKEN_SCOPE,
        });
        let flow = OpenAIChatGPTOAuth::new(transport.clone()).with_callback_port(port);
        let result = login_with_callback(&flow, &fake, None, false, None).await;
        assert_ne!(fake.callback(false).port(), Some(port));
        if succeeds {
            assert_eq!(
                result.unwrap().get_extra_str("clientId"),
                Some(ISSUED_CLIENT)
            );
            assert_eq!(transport.scripted.requests().len(), 3);
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("The pasted callback URL must start with")
            );
            assert!(transport.scripted.requests().is_empty());
        }
    }
}

#[tokio::test]
async fn distinguishes_state_bound_consent_declines_from_unrelated_callbacks() {
    for kind in [CallbackKind::Manual, CallbackKind::Loopback] {
        let (flow, fake, transport) =
            fixture(kind, None, true, "test-subject", DIRECT_TOKEN_SCOPE, 0);
        let error = login_with_callback(
            &flow,
            &fake,
            None,
            matches!(kind, CallbackKind::Loopback),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.is::<OpenAIConsentDeclinedError>());
        assert!(transport.scripted.requests().is_empty());
    }
    let error = authorization_result_from_manual_input(
        &format!("{REDIRECT_URI}?state=wrong&error=access_denied"),
        "expected",
        REDIRECT_URI,
        None,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "OAuth state mismatch");
}

#[tokio::test]
async fn requests_explicit_plan_consent_only_for_its_saved_client() {
    for (callback_client, succeeds) in [
        (None, true),
        (Some(ISSUED_CLIENT), true),
        (Some("other"), false),
    ] {
        let (flow, fake, transport) = fixture(
            CallbackKind::Manual,
            callback_client,
            false,
            "test-subject",
            SCOPE,
            0,
        );
        let result = login_with_callback(
            &flow,
            &fake,
            Some(LoginOptions {
                plan_usage_consent: Some(PlanUsageConsent {
                    client_id: ISSUED_CLIENT.into(),
                }),
                ..Default::default()
            }),
            false,
            None,
        )
        .await;
        let authorization = fake.authorization();
        let query = authorization.query_pairs().collect::<HashMap<_, _>>();
        assert_eq!(query["client_id"], ISSUED_CLIENT);
        assert_eq!(query["prompt"], "consent");
        assert_eq!(query["scope"], SCOPE);
        if succeeds {
            assert_eq!(
                result.unwrap().get_extra("planUsageEnabled"),
                Some(&json!(true))
            );
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("consent callback client ID mismatch")
            );
            assert!(transport.scripted.requests().is_empty());
        }
    }
    for client_id in ["", DYNAMIC_CLIENT_ID] {
        let (flow, fake, _) = fixture(CallbackKind::Manual, None, false, "test-subject", SCOPE, 0);
        assert!(
            login_with_callback(
                &flow,
                &fake,
                Some(LoginOptions {
                    plan_usage_consent: Some(PlanUsageConsent {
                        client_id: client_id.into()
                    }),
                    ..Default::default()
                }),
                false,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("requires the saved issued client ID")
        );
        assert!(fake.urls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn retains_verified_login_without_direct_scope_but_refuses_request_auth() {
    let (flow, fake, _) = fixture(
        CallbackKind::Manual,
        Some(ISSUED_CLIENT),
        false,
        "test-subject",
        "openid",
        0,
    );
    let credential = login_with_callback(&flow, &fake, None, false, None)
        .await
        .unwrap();
    assert_eq!(credential.get_extra_str("subject"), Some("test-subject"));
    assert_eq!(
        credential.get_extra("planUsageEnabled"),
        Some(&json!(false))
    );
    assert!(
        flow.to_auth(&credential)
            .await
            .unwrap_err()
            .is::<OpenAIPlanUsageNotEnabledError>()
    );
    assert!(flow.to_auth(&saved()).await.is_ok());
    let disabled = saved().with_extra("planUsageEnabled", json!(false));
    assert!(
        flow.to_auth(&disabled)
            .await
            .unwrap_err()
            .is::<OpenAIPlanUsageNotEnabledError>()
    );
    for flag in [Value::Null, json!(false)] {
        let no_scope = saved()
            .with_extra("scopes", json!(["openid"]))
            .with_extra("planUsageEnabled", flag);
        assert!(
            flow.to_auth(&no_scope)
                .await
                .unwrap_err()
                .is::<OpenAIPlanUsageNotEnabledError>()
        );
    }
}

#[tokio::test]
async fn refresh_rechecks_plan_permission_in_both_directions_and_requires_a_refresh_token() {
    let transport = Arc::new(ScriptedTransport::new(vec![]));
    let flow = OpenAIChatGPTOAuth::new(transport.clone());
    let signal = AbortController::new().signal();
    let mut credential = saved();
    for (scope, enabled) in [("openid", false), (DIRECT_TOKEN_SCOPE, true)] {
        let mut response = tests::token();
        response["scope"] = json!(scope);
        transport.push(
            None,
            ScriptedResponse::Json {
                status: 200,
                body: response,
            },
        );
        credential = flow.refresh(&credential, &signal).await.unwrap();
        assert_eq!(
            credential.get_extra("planUsageEnabled"),
            Some(&json!(enabled))
        );
        assert_eq!(flow.to_auth(&credential).await.is_ok(), enabled);
    }
    let mut response = tests::token();
    response.as_object_mut().unwrap().remove("refresh_token");
    transport.push(
        None,
        ScriptedResponse::Json {
            status: 200,
            body: response,
        },
    );
    assert!(
        flow.refresh(&credential, &signal)
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid refresh_token")
    );
}

#[tokio::test]
async fn classifies_refresh_failure_only_from_terminal_machine_codes() {
    for (status, body, expected) in [
        (400, r#"{"error":"invalid_grant"}"#, "invalid_grant"),
        (
            400,
            r#"{"error":{"code":"refresh_token_expired"}}"#,
            "refresh_token_expired",
        ),
        (
            400,
            r#"{"error":{"type":"invalid_client"}}"#,
            "invalid_client",
        ),
        (
            400,
            r#"{"error":{"code":"refresh_token_reused","type":"invalid_request_error"}}"#,
            "refresh_token_reused",
        ),
        (
            400,
            r#"{"error":{"error":"invalid_client"}}"#,
            "invalid_client",
        ),
        (
            400,
            r#"{"error":{"error":{"code":"invalid_refresh_token"}}}"#,
            "invalid_refresh_token",
        ),
        (400, r#"{"code":"invalid_client"}"#, "invalid_client"),
        (400, r#"{"error_description":"invalid_grant"}"#, ""),
        (
            400,
            r#"{"error":"unknown_error","error_description":"invalid_grant"}"#,
            "",
        ),
        (400, "invalid_client", ""),
        (400, "", ""),
        (503, r#"{"error":{"code":"invalid_grant"}}"#, ""),
        (500, "<html>unavailable</html>", ""),
        (599, r#"{"error":"invalid_grant"}"#, ""),
        (429, "", ""),
        (400, r#"{"error":"token_expired"}"#, "token_expired"),
        (
            400,
            r#"{"error":"refresh_token_invalidated"}"#,
            "refresh_token_invalidated",
        ),
    ] {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Text {
                status,
                body: body.into(),
            },
        )]));
        let error = OpenAIChatGPTOAuth::new(transport)
            .refresh(&saved(), &AbortController::new().signal())
            .await
            .unwrap_err();
        match expected {
            "invalid_client" => assert_eq!(
                error
                    .downcast_ref::<OAuthClientConfigurationError>()
                    .unwrap()
                    .status,
                status
            ),
            "" => {
                assert_eq!(
                    error
                        .downcast_ref::<crate::utils::oauth_refresh_error::OAuthTokenEndpointError>(
                        )
                        .unwrap()
                        .status,
                    status
                );
                assert_eq!(
                    classify_oauth_refresh_failure(error.as_ref()),
                    if status == 429 || status >= 500 {
                        OAuthRefreshFailureClass::Transient
                    } else {
                        OAuthRefreshFailureClass::Permanent
                    }
                );
            }
            code => {
                let error = error
                    .downcast_ref::<OAuthReauthorizationRequiredError>()
                    .unwrap();
                assert_eq!(error.code, code);
                assert_eq!(error.status, status);
            }
        }
    }
}

#[tokio::test]
async fn authorization_code_rejection_is_not_a_refresh_reauthorization_error() {
    let transport = ScriptedTransport::new(vec![(
        None,
        ScriptedResponse::Json {
            status: 400,
            body: json!({"error":"invalid_grant"}),
        },
    )]);
    let error = request_token(
        &transport,
        &[("grant_type", "authorization_code")],
        &AbortController::new().signal(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<OAuthTokenEndpointError>()
            .unwrap()
            .status,
        400
    );
    assert!(!error.is::<OAuthReauthorizationRequiredError>());
}
