//! `tools/task/execute-continuation.test.ts`

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::default_manager;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::spawn_policy::{PlanReviewContractOutcome, SpawnPolicyDeps};
use crate::tools::task::types::TaskToolMode;
use crate::tools::task::validation::SpawnParamsInput;

/// Spawn policy that never denies and never applies a plan-review contract.
struct NoSpawnPolicy;

impl SpawnPolicyDeps for NoSpawnPolicy {
    fn invocation_gate_denial(&self, _subagent_type: &str, _session_id: &str) -> Option<String> {
        None
    }

    fn plan_review_contract_outcome(
        &self,
        _subagent_type: &str,
        _caller_prompt: &str,
        _session_id: &str,
    ) -> Option<PlanReviewContractOutcome> {
        None
    }
}

#[test]
fn given_the_task_tool_when_executed_then_it_always_starts_a_new_child_task() {
    // given
    let harness = default_manager();
    let dir = tempfile::tempdir().expect("tempdir");
    let tool = Default::default();
    let policy = NoSpawnPolicy;
    let deps = TaskExecuteDeps {
        manager: &harness.manager,
        tool: &tool,
        policy: &policy,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let ctx = TaskToolContext {
        session_id: "parent-1".to_string(),
        cwd: dir.path().to_string_lossy().into_owned(),
        ..TaskToolContext::default()
    };

    let params = SpawnParamsInput {
        prompt: Some("new work".to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        ..SpawnParamsInput::default()
    };

    // when
    let result = execute
        .execute("c", &params, None, None, &ctx)
        .expect("execute succeeds");

    // then
    assert_eq!(result.details.mode, TaskToolMode::Spawn);
    let record = harness
        .manager
        .get(&result.details.task_id)
        .expect("a new child task was started");
    assert_eq!(record.task_id, result.details.task_id);
}
