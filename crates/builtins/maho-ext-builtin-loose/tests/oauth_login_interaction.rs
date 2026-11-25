use maho_ai::auth::types::{AuthInteraction, AuthPrompt, AuthPromptKind, AuthPromptOption};
use maho_ai::utils::abort::AbortController;
use maho_ext_api::*;
use maho_ext_builtin_loose::oauth_login_interaction::{ExtensionLoginInteraction, LoginCancelled};
use std::sync::Arc;

struct DialogUi {
    answer: Option<String>,
}
impl ExtensionUi for DialogUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { self.answer.clone() })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        panic!("unexpected confirm")
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { self.answer.clone() })
    }
    fn notify(&self, _: &str, _: NotificationType) {
        panic!("unexpected notification")
    }
    fn set_status(&self, _: &str, _: Option<&str>) {
        panic!("unexpected status")
    }
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {
        panic!("unexpected widget")
    }
    fn set_header(&self, _: Option<ComponentFactory>) {
        panic!("unexpected header")
    }
    fn set_footer(&self, _: Option<ComponentFactory>) {
        panic!("unexpected footer")
    }
    fn set_title(&self, _: &str) {
        panic!("unexpected title")
    }
    fn paste_to_editor(&self, _: &str) {
        panic!("unexpected editor")
    }
    fn set_editor_text(&self, _: &str) {
        panic!("unexpected editor")
    }
    fn get_editor_text(&self) -> String {
        panic!("unexpected editor")
    }
    fn custom(
        &self,
        _: ComponentFactory,
        _: CustomUiOptions,
    ) -> ExtensionFuture<'_, serde_json::Value> {
        panic!("unexpected custom UI")
    }
    fn theme(&self) -> Theme {
        panic!("unexpected theme")
    }
}
fn interaction(answer: Option<&str>, provider: Option<&str>) -> ExtensionLoginInteraction {
    ExtensionLoginInteraction::new(
        Arc::new(DialogUi {
            answer: answer.map(str::to_owned),
        }),
        ExtensionMode::Rpc,
        "Test".into(),
        provider,
        Arc::new(|_| panic!("unexpected browser")),
    )
}
fn text_prompt() -> AuthPrompt {
    AuthPrompt {
        kind: AuthPromptKind::Text {
            message: "Code".into(),
            placeholder: None,
        },
        signal: None,
    }
}

#[tokio::test]
async fn select_maps_label_to_provider_option_id() {
    let login = interaction(Some("Visible label"), None);
    let prompt = AuthPrompt {
        kind: AuthPromptKind::Select {
            message: "Choose".into(),
            options: vec![AuthPromptOption {
                id: "machine-id".into(),
                label: "Visible label".into(),
                description: None,
            }],
        },
        signal: None,
    };
    let answer = login.prompt(prompt).await.expect("answer");
    assert_eq!(answer, "machine-id");
}

#[tokio::test]
async fn dismissing_owned_dialog_aborts_login() {
    let login = interaction(None, None);
    let result = login.prompt(text_prompt()).await;
    assert!(result.unwrap_err().is::<LoginCancelled>());
    assert!(login.signal().expect("signal").aborted());
}

#[tokio::test]
async fn aborted_prompt_does_not_abort_login() {
    let login = interaction(Some("code"), None);
    let prompt_controller = AbortController::new();
    prompt_controller.abort(None);
    let mut prompt = text_prompt();
    prompt.signal = Some(prompt_controller.signal());
    let result = login.prompt(prompt).await;
    assert!(result.unwrap_err().is::<LoginCancelled>());
    assert!(!login.signal().expect("signal").aborted());
}

#[test]
fn reissuing_provider_login_cancels_previous_only() {
    let previous = interaction(Some("code"), Some("oauth-test-reissue"));
    let unrelated = interaction(Some("code"), Some("oauth-test-unrelated"));
    let current = interaction(Some("code"), Some("oauth-test-reissue"));
    assert!(previous.signal().expect("signal").aborted());
    assert!(!unrelated.signal().expect("signal").aborted());
    assert!(!current.signal().expect("signal").aborted());
}

#[test]
fn redact_oauth_url_strips_id_token_hint_only() {
    use maho_ext_builtin_loose::oauth_login_interaction::redact_oauth_url;
    let clean = "https://auth.openai.com/api/accounts/authorize?client_id=abc&scope=openid";
    assert_eq!(redact_oauth_url(clean), clean);
    let with_hint = "https://auth.openai.com/api/accounts/authorize?client_id=abc&id_token_hint=secret.jwt.token&scope=openid";
    let redacted = redact_oauth_url(with_hint);
    assert!(!redacted.contains("id_token_hint"));
    assert!(!redacted.contains("secret.jwt.token"));
    assert!(redacted.contains("client_id=abc"));
    assert!(redacted.contains("scope=openid"));
}

#[tokio::test]
async fn secret_prompt_sets_secret_option_when_ui_supports_it() {
    use maho_ai::auth::types::AuthEvent;
    use std::sync::Mutex;

    struct SecretUi {
        seen_secret: Mutex<Vec<bool>>,
        notified: Mutex<Vec<String>>,
    }
    impl ExtensionUi for SecretUi {
        fn select<'a>(
            &'a self,
            _: &'a str,
            _: &'a [String],
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, bool> {
            Box::pin(async { false })
        }
        fn input<'a>(
            &'a self,
            _: &'a str,
            _: Option<&'a str>,
            opts: ExtensionUiDialogOptions,
        ) -> UiFuture<'a, Option<String>> {
            self.seen_secret.lock().unwrap().push(opts.secret);
            Box::pin(async { Some("tok".into()) })
        }
        fn notify(&self, msg: &str, _: NotificationType) {
            self.notified.lock().unwrap().push(msg.into());
        }
        fn capabilities(&self) -> ExtensionUiCapabilities {
            ExtensionUiCapabilities { secret_input: true }
        }
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(
            &self,
            _: ComponentFactory,
            _: CustomUiOptions,
        ) -> ExtensionFuture<'_, serde_json::Value> {
            Box::pin(async { Ok(serde_json::Value::Null) })
        }
        fn theme(&self) -> Theme {
            Theme::default()
        }
    }

    let ui = Arc::new(SecretUi {
        seen_secret: Mutex::default(),
        notified: Mutex::default(),
    });
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let opened_cb = Arc::clone(&opened);
    let login = ExtensionLoginInteraction::new(
        ui.clone(),
        ExtensionMode::Tui,
        "OpenAI".into(),
        None,
        Arc::new(move |url| opened_cb.lock().unwrap().push(url.into())),
    );
    let answer = login
        .prompt(AuthPrompt {
            kind: AuthPromptKind::Secret {
                message: "API key".into(),
                placeholder: None,
            },
            signal: None,
        })
        .await
        .unwrap();
    assert_eq!(answer, "tok");
    assert_eq!(*ui.seen_secret.lock().unwrap(), [true]);
    let raw_url = "https://auth.example.com/authorize?id_token_hint=topsecret&code=1";
    login.notify(AuthEvent::AuthUrl {
        url: raw_url.into(),
        instructions: None,
    });
    assert_eq!(*opened.lock().unwrap(), [raw_url]);
    let msg = ui.notified.lock().unwrap()[0].clone();
    assert!(!msg.contains("topsecret"));
}
