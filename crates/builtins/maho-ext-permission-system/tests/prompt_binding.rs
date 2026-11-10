//! senpi `showPermissionPrompt` binding: a dialog names the call it approves, and the wider
//! "Allow always" scope (senpi#3403) is a separate labelled choice.

use super::context_support::{TestSession, context_with_ui};
use maho_ext_api::{
    ExtensionUi, ExtensionUiDialogOptions, JsonValue, NotificationType, Theme, UiFuture,
};
use maho_ext_permission_system::{
    parsers::{PermissionRequest, create_builtin_parser_registry},
    prompt::show_permission_prompt,
    types::{Reply, Request, RequestTool, WiderAlways},
};
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

struct RecordingUi {
    answer: Option<String>,
    offered: Mutex<Vec<String>>,
    dialogs: Mutex<Vec<ExtensionUiDialogOptions>>,
}

impl RecordingUi {
    fn new(answer: Option<&str>) -> Self {
        Self {
            answer: answer.map(str::to_owned),
            offered: Mutex::new(Vec::new()),
            dialogs: Mutex::new(Vec::new()),
        }
    }
    fn offered(&self) -> Vec<String> {
        self.offered.lock().expect("offered").clone()
    }
    fn dialogs(&self) -> Vec<ExtensionUiDialogOptions> {
        self.dialogs.lock().expect("dialogs").clone()
    }
}

impl ExtensionUi for RecordingUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        options: &'a [String],
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        *self.offered.lock().expect("offered") = options.to_vec();
        self.dialogs.lock().expect("dialogs").push(opts);
        Box::pin(async move { self.answer.clone() })
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
        self.dialogs.lock().expect("dialogs").push(opts);
        Box::pin(async { Some("because".to_owned()) })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Ok(JsonValue::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn ui_context(ui: Arc<RecordingUi>) -> maho_ext_api::ExtensionContext {
    context_with_ui(
        ui,
        Arc::new(TestSession("session-1".into())),
        Path::new("/workspace/project"),
    )
}

fn request() -> Request {
    let mut request = Request::new(
        "permission-1".into(),
        "session-1".into(),
        "bash".into(),
        vec!["git commit".into()],
        vec!["git *".into()],
        json!({"command": "git commit -m test"})
            .as_object()
            .cloned()
            .expect("object"),
    );
    request.tool = Some(RequestTool {
        message_id: None,
        call_id: "call-1".into(),
        parent_call_id: Some("eval-1".into()),
    });
    request
}

#[tokio::test]
async fn permission_dialog_binds_the_call_it_approves() {
    let ui = Arc::new(RecordingUi::new(Some("Allow once")));
    let reply = show_permission_prompt(&ui_context(ui.clone()), &request()).await;
    assert_eq!(reply.reply, Reply::Once);
    let dialogs = ui.dialogs();
    assert_eq!(dialogs.len(), 1);
    assert_eq!(dialogs[0].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(dialogs[0].parent_tool_call_id.as_deref(), Some("eval-1"));
}

#[tokio::test]
async fn denial_feedback_keeps_the_call_binding() {
    let ui = Arc::new(RecordingUi::new(Some("Deny with feedback")));
    let reply = show_permission_prompt(&ui_context(ui.clone()), &request()).await;
    assert_eq!(reply.reply, Reply::Reject);
    assert_eq!(reply.message.as_deref(), Some("because"));
    let dialogs = ui.dialogs();
    assert_eq!(dialogs.len(), 2);
    assert_eq!(dialogs[1].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(dialogs[1].parent_tool_call_id.as_deref(), Some("eval-1"));
}

#[tokio::test]
async fn wider_scope_is_a_separate_labelled_choice() {
    let mut request = request();
    request.wider_always = Some(WiderAlways {
        label: "Allow fetch_fixture on example.invalid always in this project".into(),
        patterns: vec!["origin=https://example.invalid".into()],
    });
    let ui = Arc::new(RecordingUi::new(Some(
        "Allow fetch_fixture on example.invalid always in this project",
    )));
    let reply = show_permission_prompt(&ui_context(ui.clone()), &request).await;
    assert_eq!(reply.reply, Reply::Always);
    assert_eq!(
        reply.always,
        Some(vec!["origin=https://example.invalid".to_owned()])
    );
    assert_eq!(
        ui.offered(),
        vec![
            "Allow once",
            "Allow always",
            "Allow fetch_fixture on example.invalid always in this project",
            "Deny",
            "Deny with feedback",
        ]
    );
}

#[tokio::test]
async fn plain_always_keeps_the_request_patterns() {
    let ui = Arc::new(RecordingUi::new(Some("Allow always")));
    let reply = show_permission_prompt(&ui_context(ui.clone()), &request()).await;
    assert_eq!(reply.reply, Reply::Always);
    assert_eq!(reply.always, None);
    assert_eq!(
        ui.offered(),
        vec!["Allow once", "Allow always", "Deny", "Deny with feedback"]
    );
}

#[test]
fn fallback_parser_offers_the_wider_url_scope() {
    let registry = create_builtin_parser_registry();
    let parsed = registry.parse(
        "fetch_fixture",
        &json!({"url": "https://example.invalid/page"}),
        (Path::new("/workspace/project"), Path::new("/home/user")),
    );
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].patterns, vec!["https://example.invalid/page"]);
    assert_eq!(parsed[0].always, vec!["https://example.invalid/page"]);
    let wider = parsed[0].wider_always.as_ref().expect("wider scope");
    assert_eq!(
        wider.label,
        "Allow fetch_fixture on example.invalid always in this project"
    );
    assert_eq!(wider.patterns, vec!["origin=https://example.invalid"]);
}

#[test]
fn parser_owned_tools_offer_no_wider_scope() {
    let registry = create_builtin_parser_registry();
    for (tool, input) in [
        ("bash", json!({"command": "git status"})),
        ("read", json!({"path": "/workspace/project/src/main.rs"})),
    ] {
        let parsed = registry.parse(
            tool,
            &input,
            (Path::new("/workspace/project"), Path::new("/home/user")),
        );
        assert!(
            parsed.iter().all(|request| request.wider_always.is_none()),
            "{tool}"
        );
    }
}

#[tokio::test]
async fn an_unbound_request_leaves_the_dialog_unbound() {
    let ui = Arc::new(RecordingUi::new(Some("Allow once")));
    let mut request = request();
    request.tool = None;
    show_permission_prompt(&ui_context(ui.clone()), &request).await;
    let dialogs = ui.dialogs();
    assert_eq!(dialogs[0].tool_call_id, None);
    assert_eq!(dialogs[0].parent_tool_call_id, None);
}

#[test]
fn permission_request_defaults_carry_no_wider_scope() {
    assert_eq!(PermissionRequest::default().wider_always, None);
}
