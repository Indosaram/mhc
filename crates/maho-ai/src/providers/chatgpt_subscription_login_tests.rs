use super::*;
use crate::auth::types::{AuthEvent, AuthInteraction};
use crate::utils::abort::AbortController;
use std::sync::Mutex;

#[derive(Default)]
struct FakeFlow {
    requests: Mutex<Vec<Option<LoginOptions>>>,
}

#[async_trait]
impl OAuthAuth for FakeFlow {
    fn name(&self) -> &str {
        "synthetic OAuth"
    }

    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        self.login_with_options(interaction, None).await
    }

    async fn login_with_options(
        &self,
        _interaction: &ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> anyhow::Result<OAuthCredential> {
        let credential = options
            .as_ref()
            .and_then(|options| options.reauthorize.clone());
        self.requests
            .lock()
            .expect("capture login options")
            .push(options);
        Ok(credential
            .unwrap_or_else(|| OAuthCredential::new("synthetic-access", "synthetic-refresh", 10.0)))
    }

    async fn refresh(
        &self,
        _: &OAuthCredential,
        _: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        anyhow::bail!("no exchange in the route-selection test")
    }

    async fn to_auth(&self, _: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        anyhow::bail!("no requests in the route-selection test")
    }
}

struct PromptCapture {
    selected: Option<&'static str>,
    prompts: Mutex<Vec<AuthPromptKind>>,
}

#[async_trait]
impl AuthInteraction for PromptCapture {
    fn signal(&self) -> Option<AbortSignal> {
        None
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        self.prompts
            .lock()
            .expect("capture prompt")
            .push(prompt.kind);
        self.selected
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("unexpected route prompt"))
    }

    fn notify(&self, _: AuthEvent) {}
}

#[tokio::test]
async fn offers_siwc_first_and_preserves_legacy_sign_in_choice() {
    for selected in ["siwc", "legacy"] {
        let siwc = Arc::new(FakeFlow::default());
        let legacy = Arc::new(FakeFlow::default());
        let oauth = SubscriptionOAuth {
            legacy: legacy.clone(),
            siwc: siwc.clone(),
        };
        let capture = Arc::new(PromptCapture {
            selected: Some(selected),
            prompts: Mutex::default(),
        });
        let interaction =
            ProviderAuthInteraction::new(AbortController::new().signal(), capture.clone());

        let credential = oauth.login(&interaction).await.expect("select login route");

        let prompts = capture.prompts.lock().expect("prompt capture");
        let [AuthPromptKind::Select { options, .. }] = prompts.as_slice() else {
            panic!("expected one route-selection prompt");
        };
        assert_eq!(
            options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["siwc", "legacy"]
        );
        assert_eq!(
            credential.get_extra_str("route"),
            (selected == "siwc").then_some("siwc")
        );
        assert_eq!(
            siwc.requests.lock().expect("SIWC calls").len(),
            usize::from(selected == "siwc")
        );
        assert_eq!(
            legacy.requests.lock().expect("legacy calls").len(),
            usize::from(selected == "legacy")
        );
    }
}

#[tokio::test]
async fn reauthorization_uses_the_selected_siwc_registration_without_prompting() {
    let siwc = Arc::new(FakeFlow::default());
    let legacy = Arc::new(FakeFlow::default());
    let oauth = SubscriptionOAuth {
        legacy: legacy.clone(),
        siwc: siwc.clone(),
    };
    let capture = Arc::new(PromptCapture {
        selected: None,
        prompts: Mutex::default(),
    });
    let interaction =
        ProviderAuthInteraction::new(AbortController::new().signal(), capture.clone());
    let registration = OAuthCredential::new("", "", 0.0)
        .with_extra("route", "siwc".into())
        .with_extra("clientId", "synthetic-issued-client".into());
    let result = oauth
        .login_with_options(
            &interaction,
            Some(LoginOptions {
                reauthorize: Some(registration.clone()),
                reauthorize_slot: Some("work".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("reauthorize selected registration");

    let calls = siwc.requests.lock().expect("SIWC calls");
    assert_eq!(calls.len(), 1);
    let options = calls[0].as_ref().expect("selected registration forwarded");
    assert_eq!(options.reauthorize.as_ref(), Some(&registration));
    assert_eq!(options.reauthorize_slot.as_deref(), Some("work"));
    assert_eq!(
        result.get_extra_str("clientId"),
        Some("synthetic-issued-client")
    );
    assert!(legacy.requests.lock().expect("legacy calls").is_empty());
    assert!(capture.prompts.lock().expect("prompt capture").is_empty());
}
