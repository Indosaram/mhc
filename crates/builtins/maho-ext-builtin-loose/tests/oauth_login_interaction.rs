use maho_ai::auth::types::{AuthInteraction, AuthPrompt, AuthPromptKind, AuthPromptOption};
use maho_ai::utils::abort::AbortController;
use maho_ext_api::*;
use maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction;
use std::sync::Arc;

struct DialogUi { answer: Option<String> }
impl ExtensionUi for DialogUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { self.answer.clone() }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { panic!("unexpected confirm") }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { self.answer.clone() }) }
    fn notify(&self, _: &str, _: NotificationType) { panic!("unexpected notification") }
    fn set_status(&self, _: &str, _: Option<&str>) { panic!("unexpected status") }
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) { panic!("unexpected widget") }
    fn set_header(&self, _: Option<ComponentFactory>) { panic!("unexpected header") }
    fn set_footer(&self, _: Option<ComponentFactory>) { panic!("unexpected footer") }
    fn set_title(&self, _: &str) { panic!("unexpected title") }
    fn paste_to_editor(&self, _: &str) { panic!("unexpected editor") }
    fn set_editor_text(&self, _: &str) { panic!("unexpected editor") }
    fn get_editor_text(&self) -> String { panic!("unexpected editor") }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, serde_json::Value> { panic!("unexpected custom UI") }
    fn theme(&self) -> Theme { panic!("unexpected theme") }
}
fn interaction(answer: Option<&str>, provider: Option<&str>) -> ExtensionLoginInteraction {
    ExtensionLoginInteraction::new(Arc::new(DialogUi { answer: answer.map(str::to_owned) }), ExtensionMode::Rpc, "Test".into(), provider, Arc::new(|_| panic!("unexpected browser")))
}
fn text_prompt() -> AuthPrompt {
    AuthPrompt { kind: AuthPromptKind::Text { message: "Code".into(), placeholder: None }, signal: None }
}

#[tokio::test]
async fn select_maps_label_to_provider_option_id() {
    let login = interaction(Some("Visible label"), None);
    let prompt = AuthPrompt { kind: AuthPromptKind::Select { message: "Choose".into(), options: vec![AuthPromptOption { id: "machine-id".into(), label: "Visible label".into(), description: None }] }, signal: None };
    let answer = login.prompt(prompt).await.expect("answer");
    assert_eq!(answer, "machine-id");
}

#[tokio::test]
async fn dismissing_owned_dialog_aborts_login() {
    let login = interaction(None, None);
    let result = login.prompt(text_prompt()).await;
    assert!(result.is_err());
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
    assert!(result.is_err());
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
