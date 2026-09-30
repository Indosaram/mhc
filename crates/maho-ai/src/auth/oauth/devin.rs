//! Port of senpi packages/ai/src/auth/oauth/devin.ts.

use crate::auth::oauth::devin_callback::{
    DEVIN_CALLBACK_PORT, DevinCallbackInput, start_devin_callback_server_on_port,
};
use crate::auth::oauth::devin_token::exchange_devin_authorization_code;
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::transport::{OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use crate::utils::abort::{AbortController, AbortSignal};
use async_trait::async_trait;
use std::sync::{Arc, LazyLock, Mutex};
use url::Url;

const AUTHORIZE_URL: &str = "https://app.devin.ai/auth/cli/continue";
const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1000;

fn parse_authorization_input(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(url) = Url::parse(value) {
        return url.query_pairs().find(|(key, _)| key == "code").map(|(_, value)| value.into_owned());
    }
    if value.contains("code=") {
        return url::form_urlencoded::parse(value.as_bytes())
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned());
    }
    Some(value.to_string())
}

fn authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    let mut url = Url::parse(AUTHORIZE_URL).expect("static authorize url");
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("response_type", "code");
    serializer.append_pair("redirect_uri", redirect_uri);
    serializer.append_pair("code_challenge", challenge);
    serializer.append_pair("code_challenge_method", "S256");
    serializer.append_pair("state", state);
    serializer.append_pair("prompt", "select_account");
    url.set_query(Some(&serializer.finish()));
    url.to_string()
}

enum ManualOutcome {
    Input(String),
    Error(String),
}

pub struct DevinOAuth {
    transport: Arc<dyn OAuthTransport>,
    callback_port: u16,
}

impl DevinOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport, callback_port: DEVIN_CALLBACK_PORT }
    }

    pub fn with_callback_port(transport: Arc<dyn OAuthTransport>, callback_port: u16) -> Self {
        Self { transport, callback_port }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }

    async fn login_devin(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let pkce = generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();
        let callback = Arc::new(
            start_devin_callback_server_on_port(
                DevinCallbackInput {
                    state: state.clone(),
                    verifier: pkce.verifier.clone(),
                    signal: interaction.signal.clone(),
                    login_timeout_ms: LOGIN_TIMEOUT_MS,
                },
                self.callback_port,
                self.transport.clone(),
            )
            .await?,
        );
        let manual_abort = AbortController::new();
        let manual_input: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let manual_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let outcome = async {
            interaction.notify(AuthEvent::Progress {
                message: format!("Listening for the Devin OAuth callback on {}", callback.callback_url),
            });
            interaction.notify(AuthEvent::AuthUrl {
                url: authorize_url(&callback.callback_url, &pkce.challenge, &state),
                instructions: Some(
                    "Sign in to Devin in your browser. If the browser is on another machine, paste the final redirect URL here."
                        .into(),
                ),
            });

            let prompt = AuthPrompt {
                kind: AuthPromptKind::ManualCode {
                    message: "Complete sign-in in your browser, or paste the authorization code / redirect URL here:"
                        .into(),
                    placeholder: Some(callback.callback_url.clone()),
                },
                signal: Some(manual_abort.signal()),
            };

            let manual_callback = callback.clone();
            let input_slot = manual_input.clone();
            let error_slot = manual_error.clone();
            let manual = async move {
                match interaction.prompt(prompt).await {
                    Ok(input) => {
                        *input_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(input.clone());
                        manual_callback.cancel_wait();
                        ManualOutcome::Input(input)
                    }
                    Err(error) => {
                        let message = error.to_string();
                        *error_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(message.clone());
                        manual_callback.cancel_wait();
                        ManualOutcome::Error(message)
                    }
                }
            };
            tokio::pin!(manual);

            let mut manual_settled: Option<ManualOutcome> = None;
            let mut callback_result: Option<anyhow::Result<Option<OAuthCredential>>> = None;
            tokio::select! {
                biased;
                result = callback.wait_for_credential() => callback_result = Some(result),
                settled = &mut manual => manual_settled = Some(settled),
            }

            let outcome = match manual_settled {
                Some(outcome) => outcome,
                None => {
                    let credential = callback_result.expect("the callback branch settled")?;
                    if let Some(error) = manual_error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                        anyhow::bail!("{error}");
                    }
                    match credential {
                        Some(credential) => return Ok(credential),
                        None => manual.await,
                    }
                }
            };

            match outcome {
                ManualOutcome::Error(message) => anyhow::bail!("{message}"),
                ManualOutcome::Input(input) => {
                    let Some(code) = parse_authorization_input(&input) else {
                        anyhow::bail!("Missing authorization code");
                    };
                    interaction.notify(AuthEvent::Progress {
                        message: "Exchanging the authorization code for a Devin token...".into(),
                    });
                    exchange_devin_authorization_code(&code, &pkce.verifier, &interaction.signal, self.transport.as_ref())
                        .await
                }
            }
        }
        .await;

        manual_abort.abort(None);
        callback.close();
        outcome
    }
}

pub fn devin_oauth() -> Arc<dyn OAuthAuth> {
    static INSTANCE: LazyLock<Arc<dyn OAuthAuth>> =
        LazyLock::new(|| Arc::new(DevinOAuth::new(default_transport())));
    INSTANCE.clone()
}

#[async_trait]
impl OAuthAuth for DevinOAuth {
    fn name(&self) -> &str {
        "Devin"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Devin")
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.login_devin(interaction).await
    }

    async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        Ok(credential.clone())
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::load::{load_devin_oauth, load_open_router_oauth};
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction};
    use base64::Engine as _;
    use serde_json::Value;
    use sha2::Digest as _;
    use std::sync::Mutex;

    fn base64url(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    fn jwt(payload: Value) -> String {
        let encode = |value: Value| base64url(value.to_string().as_bytes());
        format!(
            "{}.{}.signature",
            encode(serde_json::json!({"alg": "none", "typ": "JWT"})),
            encode(payload)
        )
    }

    struct Drive {
        authorize_url: Mutex<Option<Url>>,
        callback_status: Mutex<Option<u16>>,
    }

    struct DriveInteraction {
        signal: AbortSignal,
        drive: Arc<Drive>,
        state_override: Option<String>,
    }

    #[async_trait]
    impl AuthInteraction for DriveInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }
        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            std::future::pending::<()>().await;
            unreachable!()
        }
        fn notify(&self, event: AuthEvent) {
            let AuthEvent::AuthUrl { url, .. } = event else {
                return;
            };
            let authorize_url = Url::parse(&url).expect("authorize url");
            let redirect_uri = authorize_url
                .query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .map(|(_, value)| value.into_owned())
                .expect("redirect_uri");
            let issued = authorize_url
                .query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned())
                .expect("state");
            let state = self.state_override.clone().unwrap_or(issued);
            let drive = self.drive.clone();
            let callback_url = format!("{redirect_uri}?code=authorization-code&state={state}");
            self.drive.authorize_url.lock().unwrap_or_else(|p| p.into_inner()).replace(authorize_url);
            tokio::spawn(async move {
                let response = reqwest::get(&callback_url).await.expect("callback request");
                drive.callback_status.lock().unwrap_or_else(|p| p.into_inner()).replace(response.status().as_u16());
            });
        }
    }

    fn drive(transport: Arc<ScriptedTransport>, state_override: Option<&str>) -> (Arc<Drive>, ProviderAuthInteraction) {
        let drive = Arc::new(Drive {
            authorize_url: Mutex::new(None),
            callback_status: Mutex::new(None),
        });
        let signal = AbortController::new().signal();
        let interaction = ProviderAuthInteraction::new(
            signal.clone(),
            Arc::new(DriveInteraction { signal, drive: drive.clone(), state_override: state_override.map(str::to_string) }),
        );
        let _ = transport;
        (drive, interaction)
    }

    async fn wait_for<T: Clone>(slot: &Mutex<Option<T>>) -> T {
        for _ in 0..2000 {
            if let Some(value) = slot.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                return value;
            }
            tokio::task::yield_now().await;
        }
        panic!("value never arrived");
    }

    fn oauth(transport: Arc<ScriptedTransport>) -> DevinOAuth {
        DevinOAuth::with_callback_port(transport, 0)
    }

    #[tokio::test]
    async fn is_registered_in_the_oauth_loader_registry_beside_the_existing_flows() {
        let loaded = load_devin_oauth().await.unwrap();
        assert!(Arc::ptr_eq(&loaded, &load_devin_oauth().await.unwrap()));
        let open_router = load_open_router_oauth().await.unwrap();
        assert_eq!(open_router.name(), "OpenRouter OAuth");
        assert_eq!(loaded.name(), "Devin");
        assert_eq!(loaded.login_label(), Some("Sign in with Devin"));
    }

    #[tokio::test]
    async fn exchanges_the_callback_code_for_the_devin_token_and_derives_expiry_from_the_jwt() {
        let exp = (crate::auth::oauth::transport::system_now_ms() / 1000.0).floor() + 3_600.0;
        let token = jwt(serde_json::json!({ "exp": exp }));
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "token": token }) },
        )]));
        let (drive, interaction) = drive(transport.clone(), None);
        let oauth = oauth(transport.clone());

        let credential = oauth.login(&interaction).await.unwrap();

        assert_eq!(credential.access, token);
        assert_eq!(credential.refresh, token);
        assert_eq!(credential.expires, exp * 1000.0);
        assert_eq!(wait_for(&drive.callback_status).await, 200);
        let request = &transport.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case("accept")).map(|(_, value)| value.as_str()),
            Some("application/json")
        );
        let body: Value = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["code", "code_verifier"]);
        assert_eq!(body["code"], "authorization-code");
    }

    #[tokio::test]
    async fn builds_the_cli_authorize_url_with_pkce_s256_a_uuid_state_and_the_loopback_redirect() {
        let token = jwt(serde_json::json!({ "exp": 1_700_000_000 }));
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "token": token }) },
        )]));
        let (drive, interaction) = drive(transport.clone(), None);
        let oauth = oauth(transport.clone());

        oauth.login(&interaction).await.unwrap();

        let url = wait_for(&drive.authorize_url).await;
        assert_eq!(url.origin().ascii_serialization(), "https://app.devin.ai");
        assert_eq!(url.path(), "/auth/cli/continue");
        let params: std::collections::HashMap<_, _> =
            url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect();
        assert_eq!(params.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(params.get("prompt").map(String::as_str), Some("select_account"));
        assert_eq!(params.get("code_challenge_method").map(String::as_str), Some("S256"));
        assert!(!params.contains_key("client_id"));
        let redirect_uri = params.get("redirect_uri").expect("redirect_uri");
        assert!(redirect_uri.starts_with("http://127.0.0.1:"), "{redirect_uri}");
        assert!(redirect_uri.ends_with("/callback"), "{redirect_uri}");
        let state = params.get("state").expect("state");
        assert_eq!(state.len(), 36);
        assert_eq!(state.split('-').map(str::len).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);

        let body: Value = serde_json::from_str(transport.requests()[0].body.as_deref().unwrap()).unwrap();
        let verifier = body["code_verifier"].as_str().unwrap();
        let challenge = params.get("code_challenge").expect("code_challenge");
        let digest = sha2::Sha256::digest(verifier.as_bytes());
        assert_eq!(challenge, &base64url(&digest));
    }

    #[tokio::test]
    async fn falls_back_to_the_one_year_expiry_when_the_token_carries_no_usable_exp() {
        let token = jwt(serde_json::json!({ "sub": "user" }));
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "token": token }) },
        )]));
        let (_, interaction) = drive(transport.clone(), None);
        let oauth = oauth(transport);
        let before = crate::auth::oauth::transport::system_now_ms();

        let credential = oauth.login(&interaction).await.unwrap();

        assert!(credential.expires >= before + 31_536_000_000.0);
        assert!(credential.expires <= crate::auth::oauth::transport::system_now_ms() + 31_536_000_000.0);
    }

    #[tokio::test]
    async fn rejects_a_callback_whose_state_does_not_match_the_issued_state() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "token": jwt(serde_json::json!({"exp": 1})) }) },
        )]));
        let (drive, interaction) = drive(transport.clone(), Some("forged-state"));
        let oauth = oauth(transport);

        let error = oauth.login(&interaction).await.unwrap_err();
        assert!(error.to_string().to_lowercase().contains("state"), "{error}");
        assert_eq!(wait_for(&drive.callback_status).await, 400);
    }

    #[tokio::test]
    async fn surfaces_a_failed_token_exchange_instead_of_storing_a_broken_credential() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 403, body: serde_json::json!({ "error": "invalid_grant" }) },
        )]));
        let (drive, interaction) = drive(transport.clone(), None);
        let oauth = oauth(transport);

        let error = oauth.login(&interaction).await.unwrap_err();
        assert!(error.to_string().contains("403"), "{error}");
        assert_eq!(wait_for(&drive.callback_status).await, 502);
    }

    #[tokio::test]
    async fn rejects_a_token_response_that_is_not_usable_json() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Text { status: 200, body: "<html>gateway</html>".into() },
        )]));
        let (_, interaction) = drive(transport.clone(), None);
        let oauth = oauth(transport);

        let error = oauth.login(&interaction).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("JSON") || message.contains("token"), "{message}");
    }

    #[tokio::test]
    async fn rejects_a_200_response_that_carries_no_token() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            None,
            ScriptedResponse::Json { status: 200, body: serde_json::json!({ "ok": true }) },
        )]));
        let (_, interaction) = drive(transport.clone(), None);
        let oauth = oauth(transport);

        let error = oauth.login(&interaction).await.unwrap_err();
        assert!(error.to_string().contains("token"), "{error}");
    }

    #[tokio::test]
    async fn has_no_refresh_grant_and_never_overlays_the_login_host_onto_the_cascade_model_host() {
        let credential = OAuthCredential::new("devin-token", "devin-token", 42.0);
        let oauth = oauth(Arc::new(ScriptedTransport::default()));
        let signal = AbortController::new().signal();

        assert_eq!(oauth.refresh(&credential, &signal).await.unwrap(), credential);
        let auth = oauth.to_auth(&credential).await.unwrap();
        assert_eq!(auth.api_key.as_deref(), Some("devin-token"));
        assert!(auth.base_url.is_none());
    }
}
