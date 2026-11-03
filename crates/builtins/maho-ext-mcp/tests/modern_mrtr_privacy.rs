// senpi-core-ext-07 MCP half: presenter inheritance across runner wrapping and ephemeral private MRTR dialogs
// (upstream mcp-mrtr-presentations.ts + modern/elicitation.ts, exercised by modern-mrtr-privacy.test.ts).
use maho_ai::utils::abort::AbortController;
use maho_core::mcp_mrtr_presentations::{
    get_mcp_mrtr_presentation, inherit_mcp_mrtr_presentation, register_mcp_mrtr_presentation,
};
use maho_ext_api::*;
use maho_ext_mcp::modern::elicitation::{MrtrInputResponse, collect_mrtr_form};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// Answers scripted selections in order and records whether every dialog was ephemeral.
struct FormUi {
    selections: Mutex<Vec<&'static str>>,
    inputs: Mutex<Vec<&'static str>>,
    ephemeral: Mutex<Vec<bool>>,
}
impl FormUi {
    fn new(selections: Vec<&'static str>, inputs: Vec<&'static str>) -> Arc<Self> {
        Arc::new(Self {
            selections: Mutex::new(selections),
            inputs: Mutex::new(inputs),
            ephemeral: Mutex::default(),
        })
    }
}
impl ExtensionUi for FormUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        self.ephemeral.lock().unwrap().push(options.ephemeral);
        let mut selections = self.selections.lock().unwrap();
        let next = (!selections.is_empty()).then(|| selections.remove(0));
        Box::pin(async move { next.map(str::to_owned) })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        panic!("private MCP forms never use confirm")
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        self.ephemeral.lock().unwrap().push(options.ephemeral);
        let mut inputs = self.inputs.lock().unwrap();
        let next = (!inputs.is_empty()).then(|| inputs.remove(0));
        Box::pin(async move { next.map(str::to_owned) })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("custom UI must not be used".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

#[test]
fn a_runner_wrapper_inherits_the_host_presentation_and_keeps_the_raw_ui() {
    let host = FormUi::new(Vec::new(), Vec::new());
    let raw: Arc<dyn ExtensionUi> = host.clone();
    let signal = AbortController::new();
    register_mcp_mrtr_presentation(raw.clone(), signal.signal(), None, None);
    let wrapper: Arc<dyn ExtensionUi> = FormUi::new(Vec::new(), Vec::new());
    assert!(get_mcp_mrtr_presentation(&wrapper).is_none());
    let wrapper = inherit_mcp_mrtr_presentation(&raw, wrapper);
    let inherited = get_mcp_mrtr_presentation(&wrapper).expect("inherited presentation");
    assert!(
        Arc::ptr_eq(&inherited.ui, &raw),
        "private dialogs still use the raw host UI"
    );
    signal.abort(None);
    assert!(
        get_mcp_mrtr_presentation(&wrapper).is_none(),
        "an aborted presentation never resolves"
    );
    assert!(get_mcp_mrtr_presentation(&raw).is_none());
}

#[test]
fn an_unregistered_source_leaves_the_wrapper_without_a_presentation() {
    let source: Arc<dyn ExtensionUi> = FormUi::new(Vec::new(), Vec::new());
    let wrapper = inherit_mcp_mrtr_presentation(&source, FormUi::new(Vec::new(), Vec::new()));
    assert!(get_mcp_mrtr_presentation(&wrapper).is_none());
}

#[tokio::test]
async fn private_mrtr_form_dialogs_are_ephemeral() {
    let host = FormUi::new(vec!["Continue", "Submit"], vec!["Ada"]);
    let raw: Arc<dyn ExtensionUi> = host.clone();
    register_mcp_mrtr_presentation(raw.clone(), AbortController::new().signal(), None, None);
    let presentation = get_mcp_mrtr_presentation(&raw).expect("presentation");
    let schema =
        json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]});
    let answer = collect_mrtr_form(
        &presentation,
        "fixture",
        "Who?",
        &schema,
        &AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        answer,
        MrtrInputResponse::Accept {
            content: json!({"name":"Ada"}).as_object().unwrap().clone()
        }
    );
    let ephemeral = host.ephemeral.lock().unwrap().clone();
    assert_eq!(ephemeral.len(), 3);
    assert!(ephemeral.iter().all(|flag| *flag), "{ephemeral:?}");
}

#[tokio::test]
async fn a_declined_private_form_returns_no_field_values() {
    let host = FormUi::new(vec!["Decline"], Vec::new());
    let raw: Arc<dyn ExtensionUi> = host.clone();
    register_mcp_mrtr_presentation(raw.clone(), AbortController::new().signal(), None, None);
    let presentation = get_mcp_mrtr_presentation(&raw).expect("presentation");
    let schema = json!({"type":"object","properties":{"name":{"type":"string"}}});
    assert_eq!(
        collect_mrtr_form(
            &presentation,
            "fixture",
            "Who?",
            &schema,
            &AbortSignal::default()
        )
        .await
        .unwrap(),
        MrtrInputResponse::Decline
    );
}

#[tokio::test]
async fn an_invalid_form_schema_is_refused_before_any_dialog() {
    let host = FormUi::new(vec!["Continue"], Vec::new());
    let raw: Arc<dyn ExtensionUi> = host.clone();
    register_mcp_mrtr_presentation(raw.clone(), AbortController::new().signal(), None, None);
    let presentation = get_mcp_mrtr_presentation(&raw).expect("presentation");
    let error = collect_mrtr_form(
        &presentation,
        "fixture",
        "Who?",
        &json!({"type":"object","properties":{"x":{"type":"object"}}}),
        &AbortSignal::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.code,
        maho_ext_mcp::modern::errors::McpMrtrErrorCode::InvalidInput
    );
    assert!(host.ephemeral.lock().unwrap().is_empty());
}
