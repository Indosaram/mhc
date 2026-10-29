//! `tools/output/factory.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::types::{ListScope, ListedTask};
use crate::state::TaskRecord;
use crate::tools::output::output::{
    TASK_OUTPUT_TOOL_NAME, TaskOutputTool, create_task_output_tool, task_output_params_schema,
};
use crate::tools::output::types::{OutputManager, TaskOutputDeps};

/// `{ get: () => undefined, list: () => [] }`
struct EmptyOutputManager;

impl OutputManager for EmptyOutputManager {
    fn get(&self, _task_id: &str) -> Option<TaskRecord> {
        None
    }

    fn list(&self, _scope: &ListScope) -> Vec<ListedTask> {
        Vec::new()
    }
}

fn output_deps() -> TaskOutputDeps {
    TaskOutputDeps {
        manager: Arc::new(EmptyOutputManager),
        state_dir: "/tmp/state".to_string(),
        transcript_reader: None,
        resolve_caller_session_id: None,
        now: None,
    }
}

#[test]
fn given_the_output_factory_when_built_then_name_label_and_typebox_params_are_wired() {
    // given / when
    let output = create_task_output_tool(output_deps());

    // then
    assert_eq!(output.name, "task_output");
    assert_eq!(output.name, TASK_OUTPUT_TOOL_NAME);
    assert_eq!(output.parameters, task_output_params_schema());
    assert!(!output.label.is_empty());

    // renderCall / renderResult are wired as methods on the tool definition.
    let _render_call = TaskOutputTool::render_call;
    let _render_result = TaskOutputTool::render_result;
}
