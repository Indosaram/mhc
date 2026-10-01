//! `tools/output/renderers-run-stats.test.ts`

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::state::{ResidencyState, TaskRunStats, TaskStatus};
use crate::tools::control::tool_result::tool_result;
use crate::tools::output::renderers::{ToolRenderResultOptions, render_task_output_result};
use crate::tools::output::types::{TaskOutputDetails, TaskSnapshot};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};

struct TestTheme;

impl RendererTheme for TestTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        text.to_string()
    }

    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
}

const RESULT_OPTIONS: ToolRenderResultOptions = ToolRenderResultOptions {
    expanded: false,
    is_partial: false,
};

#[test]
fn given_a_completed_task_with_run_stats_when_the_status_row_renders_then_duration_tool_count_and_tps_stay_adjacent()
{
    // given
    let residency_state = ResidencyState::Resident;
    let run_stats: TaskRunStats = serde_json::from_value(json!({
        "runtime_ms": 134_000,
        "turns": 3,
        "tool_calls": 5,
        "output_tokens": 900,
        "total_tokens": 4_200,
        "generation_ms": 7_600,
        "tokens_per_second": 118,
    }))
    .expect("run stats should parse");
    let details = TaskOutputDetails::Status {
        snapshot: TaskSnapshot {
            task_id: "st_done".to_string(),
            name: None,
            description: None,
            task_summary: None,
            status: TaskStatus::parse("completed").expect("completed status"),
            residency_state,
            suspended: None,
            execution_mode: "in-process".to_string(),
            model: "raw-model".to_string(),
            resolved_model: None,
            agent_type: None,
            category: None,
            parent_session_id: "session-parent".to_string(),
            root_session_id: "session-root".to_string(),
            age_ms: 10,
            pid: None,
            child_session_id: None,
            final_response: None,
            error_message: None,
            run_stats: Some(run_stats),
            lost: None,
        },
    };

    // when
    let theme = TestTheme;
    let lines = render_task_output_result(&tool_result("ignored", details), &RESULT_OPTIONS, &theme).render(200);
    let line = lines.first().cloned().unwrap_or_default();

    // then
    assert_eq!(line.contains("task_output st_done completed"), true, "line: {line}");
    assert_eq!(
        line.contains("· ran 2m 14s · 5 tools · 118 tok/s"),
        true,
        "line: {line}"
    );
}
