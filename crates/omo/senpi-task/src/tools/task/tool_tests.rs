//! `tools/task/tool.test.ts`

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::agents::AgentDefinition;
use crate::manager::TaskManager;
use crate::manager::create_task_manager;
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec,
    ManagerStartSpec, PlanResolutionError, ResolvedChildPlan, TaskManagerOptions,
};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};
use crate::store::{StateDirConfig, TaskRecordStore};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};
use crate::tools::task::call_renderer::TaskCallArgs;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::foreground_wait::ForegroundWaitOptions;
use crate::tools::task::renderers::renderer_visible_width;
use crate::tools::task::tool::{
    RenderResultOptions, TASK_TOOL_NAME, TaskTool, TaskToolDeps, create_task_tool,
};
use crate::tools::task::types::{TaskToolDetails, TaskToolMode};

const ANSI_ITALIC: &str = "\u{001b}[3m";
const ANSI_ITALIC_END: &str = "\u{001b}[23m";

struct TestTheme;

impl RendererTheme for TestTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        format!("\u{001b}[36m{text}\u{001b}[39m")
    }

    fn italic(&self, text: &str) -> String {
        format!("{ANSI_ITALIC}{text}{ANSI_ITALIC_END}")
    }
}

/// Mirrors the TS fake manager: every runner entry point throws when exercised.
struct UnconfiguredRunner;

impl ManagedRunner for UnconfiguredRunner {
    fn start(&self, _spec: &ManagedStartSpec) -> ManagedRunnerResult {
        panic!("fake TaskManager.start not configured")
    }
}

fn omo_config() -> Value {
    json!({
        "categories": { "release-crew": { "description": "Ships the release train" } },
        "agents": {}
    })
}

fn agents() -> BTreeMap<String, AgentDefinition> {
    let mut agents = BTreeMap::new();
    agents.insert(
        "momus".to_string(),
        AgentDefinition {
            name: "momus".to_string(),
            description: Some("Deep reasoning".to_string()),
            ..Default::default()
        },
    );
    agents
}

fn test_manager(state_dir: &Path) -> TaskManager {
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: state_dir.to_path_buf(),
        task_state_dir: None,
    });
    let runner: Arc<dyn ManagedRunner> = Arc::new(UnconfiguredRunner);
    let runners = ManagedRunners {
        in_process: Arc::clone(&runner),
        process: runner,
    };
    let planner: ChildPlanner = Arc::new(
        |_spec: &ManagerStartSpec| -> Result<ResolvedChildPlan, Box<PlanResolutionError>> {
            panic!("fake TaskManager.start not configured")
        },
    );
    create_task_manager(TaskManagerOptions::new(
        store,
        runners,
        planner,
        state_dir.to_string_lossy().into_owned(),
    ))
}

/// Builds the real task tool over a manager that is never exercised (all TS fakes throw).
fn with_tool<R>(f: impl FnOnce(&TaskTool<'_>) -> R) -> R {
    let dir = tempfile::tempdir().expect("tempdir");
    let manager = test_manager(dir.path());
    let omo = omo_config();
    let agents = agents();
    let execute_tool = crate::tools::task::execute_spec::TaskToolDeps::default();
    let tool = create_task_tool(TaskToolDeps {
        execute: TaskExecuteDeps {
            manager: &manager,
            tool: &execute_tool,
            policy: &execute_tool,
        },
        options: ForegroundWaitOptions::default(),
        omo_config: &omo,
        agents: &agents,
    });
    f(&tool)
}

fn text_result(text: &str, details: TaskToolDetails) -> TaskToolResult {
    TaskToolResult {
        content: serde_json::from_value(json!([{ "type": "text", "text": text }]))
            .expect("text content"),
        details,
    }
}

fn running_details() -> TaskToolDetails {
    TaskToolDetails {
        task_id: "st_1".to_string(),
        status: "running".to_string(),
        mode: TaskToolMode::Spawn,
        ..Default::default()
    }
}

fn resolved_model(provider: &str, model_id: &str, display: &str) -> ResolvedModelRecord {
    ResolvedModelRecord {
        provider: provider.to_string(),
        model_id: model_id.to_string(),
        display: display.to_string(),
        reasoning_effort: Some("xhigh".to_string()),
        reasoning: None,
        variant: None,
        source: ResolvedModelSource::Category,
    }
}

fn first_row(component: &dyn LinesComponent, width: usize) -> String {
    component.render(width).into_iter().next().unwrap_or_default()
}

#[test]
fn given_deps_when_the_tool_is_created_then_it_exposes_the_senpi_tool_definition_surface() {
    with_tool(|tool| {
        assert_eq!(tool.name, TASK_TOOL_NAME);
        assert!(!tool.label.is_empty());
        assert_eq!(tool.parameters["type"], json!("object"));
        assert!(!tool.prompt_snippet.is_empty());
        // promptGuidelines is a list; execute/renderCall/renderResult are methods on TaskTool.
        let _guidelines: &Vec<String> = &tool.prompt_guidelines;
    });
}

#[test]
fn given_a_custom_omo_json_category_when_the_description_is_read_then_it_lists_that_category() {
    with_tool(|tool| {
        assert!(tool.description.contains("release-crew"));
        assert!(tool.description.contains("Ships the release train"));
        assert!(tool.description.contains("momus"));
    });
}

#[test]
fn given_the_assembled_tool_when_parameters_are_read_then_prompt_and_tasks_are_optional() {
    with_tool(|tool| {
        assert!(tool.parameters.get("required").is_none());
    });
}

#[test]
fn given_the_real_task_call_renderer_when_rendered_at_72_columns_then_prompt_and_italic_background_are_visible()
 {
    with_tool(|tool| {
        let theme = TestTheme;
        let args = TaskCallArgs {
            prompt: Some(
                "실제 프롬프트입니다. This extra text forces a concise excerpt in the task row."
                    .to_string(),
            ),
            category: Some("quick".to_string()),
            run_in_background: Some(true),
            ..Default::default()
        };
        let component = tool.render_call(&args, &theme);
        let row = first_row(component.as_ref(), 72);

        assert!(row.contains("실제 프롬프트"));
        assert!(row.contains(&format!("{ANSI_ITALIC}background{ANSI_ITALIC_END}")));
        assert!(renderer_visible_width(&row) <= 72);
    });
}

#[test]
fn given_a_category_task_call_when_rendered_then_the_call_row_is_prompt_only() {
    with_tool(|tool| {
        let theme = TestTheme;
        let args = TaskCallArgs {
            prompt: Some("Inspect task rendering".to_string()),
            category: Some("quick".to_string()),
            run_in_background: Some(false),
            ..Default::default()
        };
        let component = tool.render_call(&args, &theme);
        let row = first_row(component.as_ref(), 120);

        assert!(row.contains("task \"Inspect task rendering\""));
        assert!(!row.contains("quick"));
        assert!(!row.contains("category:"));
        assert!(row.contains(&format!("{ANSI_ITALIC}foreground{ANSI_ITALIC_END}")));
    });
}

#[test]
fn given_an_agent_task_call_when_rendered_then_the_call_row_is_prompt_only_without_agent_target() {
    with_tool(|tool| {
        let theme = TestTheme;
        let args = TaskCallArgs {
            prompt: Some("Inspect task rendering".to_string()),
            subagent_type: Some("atlas".to_string()),
            run_in_background: Some(false),
            ..Default::default()
        };
        let component = tool.render_call(&args, &theme);
        let row = first_row(component.as_ref(), 120);

        assert!(row.contains("task \"Inspect task rendering\""));
        assert!(!row.contains("agent:atlas"));
    });
}

#[test]
fn given_a_partial_child_progress_result_when_rendered_then_only_the_last_line_row_renders() {
    with_tool(|tool| {
        let theme = TestTheme;
        let result = text_result("↳ last: found it", running_details());
        let component =
            tool.render_result(&result, RenderResultOptions { is_partial: true }, &theme);

        assert_eq!(
            component.render(120),
            vec!["\u{001b}[36m↳ last: found it\u{001b}[39m".to_string()]
        );
    });
}

#[test]
fn given_a_partial_result_with_empty_content_when_rendered_then_no_extra_rows_render() {
    with_tool(|tool| {
        let theme = TestTheme;
        let result = text_result("", running_details());
        let component =
            tool.render_result(&result, RenderResultOptions { is_partial: true }, &theme);

        assert_eq!(component.render(120), Vec::<String>::new());
    });
}

#[test]
fn given_the_real_task_result_renderer_when_a_category_result_is_rendered_then_resolved_context_and_foreground_are_visible()
 {
    with_tool(|tool| {
        let theme = TestTheme;
        let details = TaskToolDetails {
            task_id: "st_0000000f".to_string(),
            status: "pending".to_string(),
            mode: TaskToolMode::Spawn,
            category: Some("quick".to_string()),
            resolved_model: Some(resolved_model("openai", "gpt-5.6-sol", "GPT-5.6 Sol")),
            run_in_background: Some(false),
            ..Default::default()
        };
        let result = text_result("queued", details);
        let component =
            tool.render_result(&result, RenderResultOptions { is_partial: false }, &theme);
        let row = first_row(component.as_ref(), 72);

        assert!(row.contains("category:quick(openai/gpt-5.6-sol:xhigh)"));
        assert!(row.contains(&format!("{ANSI_ITALIC}foreground{ANSI_ITALIC_END}")));
        assert!(renderer_visible_width(&row) <= 72);
    });
}

#[test]
fn given_an_ultrabrain_background_result_when_rendered_at_72_columns_then_every_required_context_token_remains_complete()
 {
    with_tool(|tool| {
        let theme = TestTheme;
        let details = TaskToolDetails {
            task_id: "st_019f4d02".to_string(),
            status: "running".to_string(),
            mode: TaskToolMode::Spawn,
            category: Some("ultrabrain".to_string()),
            resolved_model: Some(resolved_model("omo-mock", "mock-1", "omo-mock/mock-1")),
            run_in_background: Some(true),
            ..Default::default()
        };
        let result = text_result("running", details);
        let component =
            tool.render_result(&result, RenderResultOptions { is_partial: false }, &theme);
        let row = first_row(component.as_ref(), 72);

        assert!(row.contains("category:ultrabrain"));
        assert!(row.contains("omo-mock/mock-1"));
        assert!(row.contains("xhigh"));
        assert!(row.contains(&format!("{ANSI_ITALIC}background{ANSI_ITALIC_END}")));
        assert!(row.contains("running"));
        assert!(!row.contains("backgrou..."));
        assert!(renderer_visible_width(&row) <= 72);
    });
}
