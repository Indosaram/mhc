use crate::{
    component_list::{options, slot},
    support::{manual_runtime_options, new_api},
};
use maho_codemode::{
    bridges::schema_bridge::EvalSchemaToolInfo,
    extension::kernel_preludes::active_kernel_preludes,
    kernels::{
        js::context_manager::JavaScriptKernel,
        shared::{subprocess_contract::KernelRunInput, subprocess_process::ProcessError},
    },
};
use maho_ext_api::{
    Extension, RegisteredTool, ToolExecutionMode, ToolExposure, normalize_tool_exposure,
};
use maho_omo::{OmoExtension, RecordingLogger, omo_components};
use serde_json::json;
use std::{path::Path, sync::Arc, time::Duration};

const EXIT_ERROR: &str = "ProcessExitInCell";
const DEADLINE: Duration = Duration::from_secs(10);

fn register_guard() -> RegisteredTool {
    let component = omo_components(slot("task"), slot("memory"), &options())
        .into_iter()
        .find(|component| component.name == "js-kernel-guard")
        .expect("production list contains js-kernel-guard");
    let extension = OmoExtension::with_options(
        vec![component],
        manual_runtime_options(Arc::new(RecordingLogger::new())),
        Default::default(),
    );
    let mut api = new_api();
    extension.register(&mut api);
    assert_eq!(api.registered.tools.len(), 1);
    api.registered.tools.pop().expect("registered guard tool")
}

#[test]
fn registers_eval_only_guard_tool_with_prelude() {
    let tool = register_guard().definition;
    assert_eq!(tool.name, "js_kernel_guard");
    assert_eq!(tool.label, "JS kernel guard");
    assert_eq!(tool.exposure, Some(ToolExposure::Eval));
    assert!(matches!(
        tool.execution_mode,
        Some(ToolExecutionMode::Parallel)
    ));
    assert_eq!(
        tool.parameters,
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    );
    let prelude = tool.kernel_prelude.expect("guard contributes a prelude");
    assert_eq!(prelude.exports, ["omoJsKernelGuard"]);
}

async fn assert_guarded_exit(cwd: &Path, code: &str, exit_code: u8) {
    let tool = register_guard();
    let real_tool_info = normalize_tool_exposure(&tool.definition, tool.source_info);
    let preludes = active_kernel_preludes(&[], &[EvalSchemaToolInfo::from(real_tool_info)]);
    let kernel = tokio::time::timeout(
        DEADLINE,
        JavaScriptKernel::start(cwd, "js-kernel-guard", 2, None),
    )
    .await
    .expect("bounded JS kernel startup")
    .expect("start JS kernel");

    let run = |id: &str, code: &str| {
        kernel.run_with_preludes(
            KernelRunInput {
                cell_id: id.into(),
                code: code.into(),
                timeout_ms: Some(5000),
            },
            None,
            None,
            &preludes,
        )
    };
    let cells = tokio::time::timeout(DEADLINE, async {
        let before = run(
            "before",
            "globalThis.o16Marker = 41; globalThis.omoJsKernelGuard",
        )
        .await?;
        let exit = run("exit", code).await?;
        let after = run(
            "after",
            "[globalThis.omoJsKernelGuard, globalThis.o16Marker]",
        )
        .await?;
        Ok::<_, ProcessError>((before, exit, after))
    })
    .await;
    tokio::time::timeout(DEADLINE, kernel.close())
        .await
        .expect("bounded JS kernel teardown")
        .expect("close JS kernel after guarded exit");

    let (before, exit, after) = cells
        .expect("bounded JS cells")
        .expect("settled JS cell results");
    assert_eq!(before["ok"], true, "{before}");
    assert_eq!(before["valueRepr"], "true", "{before}");
    assert_eq!(exit["ok"], false, "{exit}");
    assert_eq!(exit["error"]["name"], EXIT_ERROR, "{exit}");
    assert_eq!(
        exit["error"]["message"],
        format!(
            "process.exit({exit_code}) ran inside a JavaScript cell. The kernel stayed up and its globals were kept."
        ),
        "{exit}"
    );
    assert_eq!(after["ok"], true, "{after}");
    assert_eq!(after["valueRepr"], "[true,41]", "{after}");
}

#[tokio::test]
async fn direct_process_exit_throws_and_preserves_kernel_globals() {
    let cwd = tempfile::tempdir().expect("isolated kernel cwd");
    assert_guarded_exit(cwd.path(), "process.exit(23)", 23).await;
}

#[tokio::test]
async fn imported_module_exit_throws_and_preserves_kernel_globals() {
    let cwd = tempfile::tempdir().expect("isolated kernel cwd");
    std::fs::write(
        cwd.path().join("attempt-exit.mjs"),
        r#"
try {
  throw new Error('module setup failed');
} catch {
  process.exit();
}
"#,
    )
    .expect("write local module fixture");
    assert_guarded_exit(cwd.path(), "await import('./attempt-exit.mjs')", 0).await;
}
