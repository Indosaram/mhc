use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use maho_codemode::{
    bridges::output_bridge::OutputExecuteTool,
    bridges::schema_bridge::EvalSchemaToolInfo,
    config::settings::CodemodeSettings,
    extension::kernel_preludes::{active_kernel_preludes, project_kernel_prelude, KernelPreludeCollisionError},
    extension::session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions},
    interpreters::detect::{InterpreterDetection, LanguageAvailability},
    prompt::eval_prompt::{build_eval_prompt, EvalPromptOptions},
    tool::{
        detached_cell_manager::EvalDetachedCellManager,
        eval_tool::create_eval_tool,
        eval_tool_options::*,
        types::*,
    },
};
use maho_tools::definition::{AbortSignal, KernelPrelude, ToolCall, ToolContent};
use serde_json::json;

struct TestImageSdk;
impl maho_codemode::tool::image_resize::EvalImageSdk for TestImageSdk {
    fn resize_image<'a>(&'a self, _: Vec<u8>, _: &'a str, _: Option<usize>) -> maho_codemode::tool::image_resize::ImageFuture<'a, Option<maho_codemode::tool::image_resize::ResizedImage>> {
        Box::pin(async { Ok(None) })
    }
    fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> maho_codemode::tool::image_resize::ImageFuture<'a, Option<maho_codemode::tool::image_resize::EvalImageContent>> {
        Box::pin(async { Ok(None) })
    }
}

struct TestExecutor {
    calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

impl OutputExecuteTool for TestExecutor {
    fn execute_tool<'a>(
        &'a self,
        tool_name: &'a str,
        params: serde_json::Value,
        _options: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        self.calls.lock().expect("test executor calls lock").push((tool_name.to_string(), params.clone()));
        Box::pin(async move {
            let mut result = maho_ext_api::AgentToolResult::text("Executed computer");
            result.details = json!({ "ok": true, "value": "called" });
            Ok(result)
        })
    }
}

fn create_computer_prelude() -> KernelPrelude {
    KernelPrelude {
        javascript: "globalThis.computer = { click: async (args) => { const res = await tool.computer({ action: 'call', method: 'click', ...args }); return res.details?.value; } };".into(),
        python: "class _Computer:\n    def click(self, **args):\n        return tool.computer({'action': 'call', 'method': 'click', **args})['details']['value']\ncomputer = _Computer()\n".into(),
        documentation: "computer(args): Drive desktop GUI".into(),
        exports: vec!["computer".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn test_active_tool_kernel_prelude_js_and_python_execution() {
    let artifacts = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let executor = Arc::new(TestExecutor { calls: calls.clone() });

    let active_tools = Arc::new(Mutex::new(vec!["computer".to_string()]));
    let computer_prelude = create_computer_prelude();
    let all_tools = vec![
        EvalSchemaToolInfo {
            name: "computer".into(),
            description: Some("Drive desktop GUI".into()),
            parameters: Some(json!({"type": "object"})),
            kernel_prelude: Some(computer_prelude.clone()),
        },
    ];

    let all_tools_catalog = all_tools.clone();
    let session = Arc::new(CodemodeSessionManager::start(CreateCodemodeSessionManagerOptions {
        session_id: "test-prelude-session".into(),
        cwd: artifacts.path().into(),
        settings: CodemodeSettings::default(),
        availability: [
            (EvalLanguage::Py, true),
            (EvalLanguage::Js, true),
            (EvalLanguage::Rb, false),
            (EvalLanguage::Jl, false),
        ].map(|(language, enabled)| (language, LanguageAvailability {
            enabled,
            detected: if language == EvalLanguage::Py {
                InterpreterDetection::Detected { path: "python3".into(), version: "3".into(), resolved_path: None }
            } else {
                InterpreterDetection::Unavailable
            },
        })),
        local_roots: None,
        artifacts_dir: Some(artifacts.path().into()),
        session_env: None,
        executor: executor.clone(),
        list_tools: Some(Arc::new(move || Ok(all_tools_catalog.clone()))),
        complete: Arc::new(|request| Box::pin(async move {
            Ok(json!({"text": request.prompt, "details": {"model": "fixture/model"}}))
        })),
    }).await.unwrap());

    let active_tools_clone = active_tools.clone();
    let all_tools_clone = all_tools.clone();
    let active_preludes_provider = Arc::new(move || {
        let active = active_tools_clone.lock().unwrap().clone();
        Ok(active_kernel_preludes(&active, &all_tools_clone))
    });

    let cells = Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default())));
    let options = Arc::new(CreateEvalToolOptions {completion_handles:std::sync::Arc::new(Default::default()),eval_handle_host:None,owner_session_id:None,
        kernel_manager: session.clone(),
        executor: executor.clone(),
        list_tools: None,
        complete: None,
        settings: Default::default(),
        artifacts_dir: Some(artifacts.path().into()),
        image_sdk: Arc::new(TestImageSdk),
        cell_manager: cells,
        on_cell_settled: None,
        prompt: Default::default(),
        runtimes: HashMap::new(),
        mode: "print".into(),
        active_kernel_preludes: Some(active_preludes_provider),
    });

    let eval_tool = create_eval_tool(options).unwrap();

    let js_call = (eval_tool.execute)(ToolCall {
        id: "call-js-1",
        params: json!({
            "language": "js",
            "code": "typeof globalThis.computer === 'object' && typeof globalThis.computer.click === 'function'",
            "summary": "check js computer prelude"
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    }).await.unwrap();

    assert!(js_call.content.iter().any(|c| matches!(c, ToolContent::Text { text, .. } if text.contains("true"))));

    let js_invoke = (eval_tool.execute)(ToolCall {
        id: "call-js-2",
        params: json!({
            "language": "js",
            "code": "await computer.click({ x: 100, y: 200 })",
            "summary": "invoke computer facade"
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    }).await.unwrap();

    assert!(js_invoke.content.iter().any(|c| matches!(c, ToolContent::Text { text, .. } if text.contains("called"))));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0].0, "computer");

    let py_call = (eval_tool.execute)(ToolCall {
        id: "call-py-1",
        params: json!({
            "language": "py",
            "code": "hasattr(computer, 'click')",
            "summary": "check py computer prelude"
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    }).await.unwrap();

    assert!(py_call.content.iter().any(|c| matches!(c, ToolContent::Text { text, .. } if text.contains("True"))));

    let py_invoke = (eval_tool.execute)(ToolCall {
        id: "call-py-invoke",
        params: json!({
            "language": "py",
            "code": "computer.click(x=10, y=20)",
            "summary": "invoke python computer facade through real bridge"
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    }).await.unwrap();

    assert!(py_invoke.content.iter().any(|c| matches!(c, ToolContent::Text { text, .. } if text.contains("called"))));
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(calls.lock().unwrap()[1].0, "computer");

    *active_tools.lock().unwrap() = vec![];

    let js_deactivated = (eval_tool.execute)(ToolCall {
        id: "call-js-3",
        params: json!({
            "language": "js",
            "code": "typeof globalThis.computer",
            "summary": "check deactivation removes js global"
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    }).await.unwrap();

    assert!(js_deactivated.content.iter().any(|c| matches!(c, ToolContent::Text { text, .. } if text.contains("undefined"))));

    let py_deactivated = (eval_tool.execute)(ToolCall {
        id: "call-py-2",
        params: json!({
            "language": "py",
            "code": "'computer' in globals()",
            "summary": "check deactivation removes py global"
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    }).await.unwrap();

    assert!(py_deactivated.content.iter().any(|c| matches!(c, ToolContent::Text { text, .. } if text.contains("False"))));

    session.dispose().await.unwrap();
}

#[test]
fn test_collision_prevention_on_builtins() {
    let collision = KernelPrelude {
        javascript: "".into(),
        python: "".into(),
        documentation: "".into(),
        exports: vec!["print".into()],
        ..Default::default()
    };
    let err = project_kernel_prelude("bad_tool", Some(collision)).unwrap_err();
    assert_eq!(err, KernelPreludeCollisionError {
        tool_name: "bad_tool".into(),
        colliding_export: "print".into(),
    });
}

#[test]
fn test_prompt_includes_active_prelude_documentation() {
    let options = EvalPromptOptions { kernel_preludes: vec![create_computer_prelude()], ..Default::default() };
    let enabled = maho_codemode::config::settings::Languages {
        js: true,
        py: true,
        rb: false,
        jl: false,
    };
    let prompt = build_eval_prompt(&enabled, &options).unwrap();
    assert!(prompt.description.contains("computer(args): Drive desktop GUI"));
}
