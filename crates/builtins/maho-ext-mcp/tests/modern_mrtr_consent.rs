//! Tests ported faithfully from ~/.cache/sync-5152/senpi-target/packages/coding-agent/test/mcp/modern-mrtr-consent.test.ts
//! covering private MRTR consent form validation and elicitation collection.

use maho_ai::utils::abort::AbortSignal as HostSignal;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::AbortSignal;
use maho_ext_api::*;
use maho_ext_mcp::modern::{
    elicitation::{MrtrInputResponse, collect_mrtr_form},
    errors::McpMrtrError,
    form_schema::validate_mrtr_form,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A scripted UI for testing MRTR consent forms that records method calls and returns pre-configured answers.
struct ScriptedUi {
    select_answers: Mutex<Vec<Option<String>>>,
    select_fn: Option<Arc<dyn Fn(&str, &[String]) -> Option<String> + Send + Sync>>,
    select_calls: Mutex<usize>,
    input_answers: Mutex<Vec<Option<String>>>,
    input_fn: Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>,
    input_calls: Mutex<usize>,
    confirm_calls: Mutex<usize>,
}

impl ScriptedUi {
    fn new(selects: Vec<&str>, inputs: Vec<&str>) -> Arc<Self> {
        Arc::new(Self {
            select_answers: Mutex::new(selects.into_iter().map(|s| Some(s.to_string())).collect()),
            select_fn: None,
            select_calls: Mutex::new(0),
            input_answers: Mutex::new(inputs.into_iter().map(|s| Some(s.to_string())).collect()),
            input_fn: None,
            input_calls: Mutex::new(0),
            confirm_calls: Mutex::new(0),
        })
    }

    fn with_select_fn(
        select_fn: impl Fn(&str, &[String]) -> Option<String> + Send + Sync + 'static,
        inputs: Vec<&str>,
    ) -> Arc<Self> {
        Arc::new(Self {
            select_answers: Mutex::default(),
            select_fn: Some(Arc::new(select_fn)),
            select_calls: Mutex::new(0),
            input_answers: Mutex::new(inputs.into_iter().map(|s| Some(s.to_string())).collect()),
            input_fn: None,
            input_calls: Mutex::new(0),
            confirm_calls: Mutex::new(0),
        })
    }

    fn select_calls(&self) -> usize {
        *self.select_calls.lock().unwrap()
    }

    fn input_calls(&self) -> usize {
        *self.input_calls.lock().unwrap()
    }

    fn confirm_calls(&self) -> usize {
        *self.confirm_calls.lock().unwrap()
    }
}

impl ExtensionUi for ScriptedUi {
    fn select<'a>(
        &'a self,
        title: &'a str,
        choices: &'a [String],
        _options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        *self.select_calls.lock().unwrap() += 1;
        if let Some(func) = &self.select_fn {
            let res = func(title, choices);
            return Box::pin(async move { res });
        }
        let mut answers = self.select_answers.lock().unwrap();
        let next = if answers.is_empty() {
            None
        } else {
            answers.remove(0)
        };
        Box::pin(async move { next })
    }

    fn confirm<'a>(
        &'a self,
        _title: &'a str,
        _message: &'a str,
        _options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        *self.confirm_calls.lock().unwrap() += 1;
        Box::pin(async { false })
    }

    fn input<'a>(
        &'a self,
        title: &'a str,
        _placeholder: Option<&'a str>,
        _options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        *self.input_calls.lock().unwrap() += 1;
        if let Some(func) = &self.input_fn {
            let res = func(title);
            return Box::pin(async move { res });
        }
        let mut answers = self.input_answers.lock().unwrap();
        let next = if answers.is_empty() {
            None
        } else {
            answers.remove(0)
        };
        Box::pin(async move { next })
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

async fn run_elicitation_form(
    ui: Arc<ScriptedUi>,
    message: &str,
    properties_schema: Value,
) -> Result<MrtrInputResponse, McpMrtrError> {
    let mut schema_map = serde_json::Map::new();
    schema_map.insert("type".into(), json!("object"));
    if let Value::Object(props) = properties_schema {
        schema_map.extend(props);
    }
    let schema = Value::Object(schema_map);
    let presentation = McpMrtrPresentation {
        ui,
        signal: HostSignal::default(),
        admit: None,
        presented: None,
        private_forms: None,
    };
    collect_mrtr_form(
        &presentation,
        "fixture",
        message,
        &schema,
        &AbortSignal::default(),
    )
    .await
}

/// Upstream case: `rejects invalid calendar defaults (%s)` in `modern-mrtr-consent.test.ts`
#[test]
fn rejects_invalid_calendar_defaults() {
    for value in ["2026-02-30T12:00:00Z", "2026-10-24T24:00:00Z"] {
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "type": "string",
                    "format": "date-time",
                    "default": value
                }
            }
        });
        assert!(
            !validate_mrtr_form(&schema),
            "expected default {value} to be rejected"
        );
    }
}

/// Upstream case: `empty forms still require explicit submission` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn empty_forms_still_require_explicit_submission() {
    let ui = ScriptedUi::new(vec![], vec![]);
    let result = run_elicitation_form(ui.clone(), "question", json!({"properties": {}})).await;
    assert_eq!(result.unwrap(), MrtrInputResponse::Cancel);
    assert!(ui.select_calls() > 0);
}

/// Upstream case: `false is data and boolean confirmation is never used` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn false_is_data_and_boolean_confirmation_is_never_used() {
    let ui = ScriptedUi::new(vec!["Continue", "false", "Submit"], vec![]);
    let result = run_elicitation_form(
        ui.clone(),
        "question",
        json!({
            "properties": { "flag": { "type": "boolean" } },
            "required": ["flag"]
        }),
    )
    .await
    .unwrap();
    let expected_content = serde_json::Map::from_iter([("flag".into(), json!(false))]);
    assert_eq!(
        result,
        MrtrInputResponse::Accept {
            content: expected_content
        }
    );
    assert_eq!(ui.confirm_calls(), 0);
}

/// Upstream case: `explicit decline has no content and collects no fields` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn explicit_decline_has_no_content_and_collects_no_fields() {
    let ui = ScriptedUi::new(vec!["Decline"], vec!["private"]);
    let result = run_elicitation_form(
        ui.clone(),
        "question",
        json!({
            "properties": { "text": { "type": "string" } }
        }),
    )
    .await
    .unwrap();
    assert_eq!(result, MrtrInputResponse::Decline);
    assert_eq!(ui.input_calls(), 0);
}

/// Upstream case: `dismissal cancels without returning partial draft content` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn dismissal_cancels_without_returning_partial_draft_content() {
    let ui = ScriptedUi::new(vec!["Continue"], vec![]);
    let result = run_elicitation_form(
        ui.clone(),
        "question",
        json!({
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }),
    )
    .await
    .unwrap();
    assert_eq!(result, MrtrInputResponse::Cancel);
}

/// Upstream case: `unsupported or invalid schemas are rejected rather than weakened` in `modern-mrtr-consent.test.ts`
#[test]
fn unsupported_or_invalid_schemas_are_rejected_rather_than_weakened() {
    let schemas = vec![
        json!({ "type": "object", "properties": { "value": { "type": "object" } } }),
        json!({ "type": "object", "properties": { "value": { "type": "string", "pattern": ".*" } } }),
        json!({ "type": "object", "properties": { "value": { "type": "string", "format": "unknown" } } }),
        json!({ "type": "object", "properties": { "value": { "type": "integer", "minimum": 10, "maximum": 1 } } }),
        json!({ "type": "object", "properties": { "value": { "type": "boolean", "default": "false" } } }),
        json!({ "type": "object", "properties": { "value": { "type": "array", "items": { "type": "object" } } } }),
    ];
    for schema in schemas {
        assert!(
            !validate_mrtr_form(&schema),
            "expected schema {schema} to be rejected"
        );
    }
}

/// Upstream case: `submitted primitive values retain their type` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn submitted_primitive_values_retain_their_type() {
    let cases = vec![
        (json!({ "type": "string" }), "", json!("")),
        (
            json!({ "type": "integer", "minimum": 0, "maximum": 100 }),
            "42",
            json!(42),
        ),
        (
            json!({ "type": "number", "minimum": 0, "maximum": 1 }),
            "0.5",
            json!(0.5),
        ),
        (
            json!({ "type": "string", "format": "email" }),
            "test@example.test",
            json!("test@example.test"),
        ),
        (
            json!({ "type": "string", "format": "date" }),
            "2026-10-24",
            json!("2026-10-24"),
        ),
        (
            json!({ "type": "string", "format": "date-time" }),
            "2026-10-24T12:00:00Z",
            json!("2026-10-24T12:00:00Z"),
        ),
    ];
    for (property, answer, expected) in cases {
        let ui = ScriptedUi::with_select_fn(
            |_title, choices| {
                if choices.iter().any(|c| c == "Submit") {
                    Some("Submit".into())
                } else {
                    choices.first().cloned()
                }
            },
            vec![answer],
        );
        let result = run_elicitation_form(
            ui,
            "question",
            json!({
                "properties": { "value": property },
                "required": ["value"]
            }),
        )
        .await
        .unwrap();
        let expected_content = serde_json::Map::from_iter([("value".into(), expected.clone())]);
        assert_eq!(
            result,
            MrtrInputResponse::Accept {
                content: expected_content
            },
            "failed for property: {property}, answer: {answer}"
        );
    }
}

/// Upstream case: `optional unset and explicit defaults differ from empty strings` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn optional_unset_and_explicit_defaults_differ_from_empty_strings() {
    let ui = ScriptedUi::new(
        vec!["Continue", "Leave unset", "Use default", "Submit"],
        vec![],
    );
    let result = run_elicitation_form(
        ui,
        "question",
        json!({
            "properties": {
                "absent": { "type": "string" },
                "flag": { "type": "boolean", "default": false }
            }
        }),
    )
    .await
    .unwrap();
    let expected_content = serde_json::Map::from_iter([("flag".into(), json!(false))]);
    assert_eq!(
        result,
        MrtrInputResponse::Accept {
            content: expected_content
        }
    );
}

/// Upstream case: `review permits editing and titled enums send constants not labels` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn review_permits_editing_and_titled_enums_send_constants_not_labels() {
    let ui = ScriptedUi::new(
        vec![
            "Continue",
            "1. Red",
            "Edit field",
            "1. color",
            "2. Green",
            "Submit",
        ],
        vec![],
    );
    let result = run_elicitation_form(
        ui,
        "question",
        json!({
            "properties": {
                "color": {
                    "type": "string",
                    "oneOf": [
                        { "const": "#f00", "title": "Red" },
                        { "const": "#0f0", "title": "Green" }
                    ]
                }
            },
            "required": ["color"]
        }),
    )
    .await
    .unwrap();
    let expected_content = serde_json::Map::from_iter([("color".into(), json!("#0f0"))]);
    assert_eq!(
        result,
        MrtrInputResponse::Accept {
            content: expected_content
        }
    );
}

/// Upstream case: `enum arrays enforce item bounds and return selected values` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn enum_arrays_enforce_item_bounds_and_return_selected_values() {
    let ui = ScriptedUi::new(
        vec!["Continue", "Done", "Try again", "1. Red", "Done", "Submit"],
        vec![],
    );
    let result = run_elicitation_form(
        ui,
        "question",
        json!({
            "properties": {
                "colors": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {
                        "anyOf": [{ "const": "#f00", "title": "Red" }]
                    }
                }
            },
            "required": ["colors"]
        }),
    )
    .await
    .unwrap();
    let expected_content = serde_json::Map::from_iter([("colors".into(), json!(["#f00"]))]);
    assert_eq!(
        result,
        MrtrInputResponse::Accept {
            content: expected_content
        }
    );
}

/// Upstream case: `invalid field values require re-entry and cannot submit partial data` in `modern-mrtr-consent.test.ts`
#[tokio::test]
async fn invalid_field_values_require_re_entry_and_cannot_submit_partial_data() {
    let ui = ScriptedUi::new(vec!["Continue", "Try again", "Submit"], vec!["2.5", "3"]);
    let result = run_elicitation_form(
        ui.clone(),
        "question",
        json!({
            "properties": {
                "value": { "type": "integer", "minimum": 0, "maximum": 5 }
            },
            "required": ["value"]
        }),
    )
    .await
    .unwrap();
    let expected_content = serde_json::Map::from_iter([("value".into(), json!(3))]);
    assert_eq!(
        result,
        MrtrInputResponse::Accept {
            content: expected_content
        }
    );
    assert_eq!(ui.input_calls(), 2);
}
