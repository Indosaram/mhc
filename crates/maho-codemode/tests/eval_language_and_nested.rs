use maho_codemode::{
    extension::cell_tool_updates::CellToolUpdates,
    tool::{
        bridge_cell_binding::bind_bridge_cell,
        eval_argument_validator::register_eval_argument_validator,
        eval_language_inference::{classify_eval_code, infer_eval_language},
        eval_tool_options::EvalKernelManager,
        nested_tool_output::{CellToolUpdateSink, NestedToolOutput},
        types::{EvalDeadlineSeconds, EvalLanguage, create_eval_input_schema},
    },
};
use maho_ext_api::AgentToolResult;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn language_inference_and_argument_validator_teach_without_branch_noise() {
    assert_eq!(
        classify_eval_code("const x = JSON.stringify({});"),
        Some("js")
    );
    assert_eq!(classify_eval_code("def foo(x):\n    return x"), Some("py"));
    assert_eq!(classify_eval_code("def foo():\n  const x = 1;"), None);
    assert_eq!(classify_eval_code("print(1)"), None);

    let inferred = infer_eval_language(Some("print(1)"), &[EvalLanguage::Js]).expect("only lang");
    assert_eq!(inferred.language, EvalLanguage::Js);

    let schema = create_eval_input_schema(
        &maho_codemode::config::settings::Languages { js: true },
        &EvalDeadlineSeconds::default(),
    )
    .expect("schema");
    register_eval_argument_validator(&schema, &[EvalLanguage::Js]);

    let tool: maho_ai::types::Tool = serde_json::from_value(json!({
        "name": "eval",
        "description": "eval",
        "parameters": schema,
    }))
    .expect("tool");

    let missing_summary = json!({"code": "1 + 1", "summary": "   "});
    let err =
        maho_ai::utils::validation::validate_tool_arguments_value(&tool, "eval", &missing_summary)
            .unwrap_err()
            .to_string();
    assert!(err.contains("eval run requires summary"), "{err}");

    let valid_omitted_lang = json!({"code": "1 + 1", "summary": "add numbers"});
    let ok = maho_ai::utils::validation::validate_tool_arguments_value(
        &tool,
        "eval",
        &valid_omitted_lang,
    );
    assert!(ok.is_ok(), "{ok:?}");
}

#[test]
fn validator_rejects_missing_code_and_passes_control_actions() {
    let schema = create_eval_input_schema(
        &maho_codemode::config::settings::Languages { js: true },
        &EvalDeadlineSeconds::default(),
    )
    .expect("schema");
    register_eval_argument_validator(&schema, &[EvalLanguage::Js]);
    let tool: maho_ai::types::Tool = serde_json::from_value(json!({
        "name": "eval",
        "description": "eval",
        "parameters": schema,
    }))
    .expect("tool");

    let missing_code = json!({"language": "js", "summary": "missing code"});
    let err =
        maho_ai::utils::validation::validate_tool_arguments_value(&tool, "eval", &missing_code)
            .unwrap_err()
            .to_string();
    assert!(err.contains("eval run requires code"), "{err}");

    let list_action = json!({"action": "list"});
    assert!(
        maho_ai::utils::validation::validate_tool_arguments_value(&tool, "eval", &list_action)
            .is_ok()
    );
}

#[test]
fn classifier_treats_print_and_conflicting_signals_as_neutral() {
    assert_eq!(classify_eval_code("print('hello')"), None);
    assert_eq!(classify_eval_code("const x = 1;\ndef y(): pass"), None);
    assert_eq!(
        classify_eval_code("import os\nprint(os.getcwd())"),
        Some("py")
    );
    assert_eq!(
        classify_eval_code("import { readFile } from 'node:fs';"),
        Some("js")
    );
}

#[test]
fn eval_language_inferred_note_identifies_basis() {
    use maho_codemode::tool::eval_language_inference::{
        EvalLanguageInferenceBasis, eval_language_inferred_note,
    };
    let only = eval_language_inferred_note(
        EvalLanguage::Js,
        EvalLanguageInferenceBasis::OnlyEnabledLanguage,
    );
    let markers =
        eval_language_inferred_note(EvalLanguage::Js, EvalLanguageInferenceBasis::CodeMarkers);
    assert!(only.contains("only-enabled-language"));
    assert!(markers.contains("code-markers"));
}

struct RecordingSink {
    events: Mutex<Vec<String>>,
}

impl CellToolUpdateSink for RecordingSink {
    fn nested_tool_update(&self, call_id: &str, partial: &AgentToolResult) {
        self.events
            .lock()
            .expect("events")
            .push(format!("update:{call_id}:{}", partial.content.len()));
    }

    fn nested_tool_settled(&self, call_id: &str) {
        self.events
            .lock()
            .expect("events")
            .push(format!("settled:{call_id}"));
    }
}

struct MockManager {
    updates: Arc<CellToolUpdates>,
}

impl EvalKernelManager for MockManager {
    fn get_kernel(
        &self,
        _: EvalLanguage,
    ) -> maho_codemode::tool::types::EvalKernelFuture<
        '_,
        std::sync::Arc<dyn maho_codemode::tool::types::EvalKernel>,
    > {
        Box::pin(async { Err("unused".into()) })
    }

    fn bind_cell_tool_updates(
        &self,
        token: &str,
        sink: Arc<dyn CellToolUpdateSink>,
    ) -> Option<Box<dyn FnOnce() + Send + Sync>> {
        Some(CellToolUpdates::bind(&self.updates, token, sink))
    }
}

#[test]
fn nested_tool_output_and_cell_tool_updates_lifecycle() {
    let mut output = NestedToolOutput::default();
    assert!(output.update("c1", &AgentToolResult::text("step 1\nstep 2")));
    assert!(!output.update("c1", &AgentToolResult::text("step 1\nstep 2")));
    assert_eq!(
        output.append_lines("cell-out", 8),
        "cell-out\nstep 1\nstep 2"
    );

    assert!(output.drop("c1"));
    // Held settled output remains visible until release_settled().
    assert_eq!(
        output.append_lines("cell-out", 8),
        "cell-out\nstep 1\nstep 2"
    );
    assert!(output.release_settled());
    assert!(!output.release_settled());
    assert_eq!(output.append_lines("cell-out", 8), "cell-out");

    output.update(
        "c2",
        &AgentToolResult::text("line 1\nline 2\nline 3\nline 4"),
    );
    assert_eq!(output.append_lines("hdr", 3), "line 2\nline 3\nline 4");
    output.clear();
    assert_eq!(output.append_lines("hdr", 3), "hdr");

    let updates = Arc::new(CellToolUpdates::default());
    let sink = Arc::new(RecordingSink {
        events: Mutex::new(Vec::new()),
    });
    let manager = MockManager {
        updates: Arc::clone(&updates),
    };
    let mut js_binding = bind_bridge_cell(&manager, EvalLanguage::Js, Arc::clone(&sink) as _);
    assert!(js_binding.token.is_none());
    js_binding.release();

    let release = CellToolUpdates::bind(&updates, "tok-1", Arc::clone(&sink) as _);
    assert!(updates.sink_for(Some("tok-1")).is_some());
    assert!(updates.sink_for(None).is_none());
    release();
    assert!(updates.sink_for(Some("tok-1")).is_none());
}
