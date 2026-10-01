//! `tools/task/execute-invocation-guard.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::agents::{PlanArtifactReference, SkillInvocationState, plan_gated_agent_names};
use crate::manager::manager_tests::fakes::default_manager;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::invocation_gate::SkillInvocationResolver;
use crate::tools::task::spawn_policy::{PlanReviewContractOutcome, SpawnPolicyDeps};
use crate::tools::task::validation::SpawnParamsInput;

/// Scripted skill-invocation state (the TS `resolverFor` return value).
#[derive(Clone, Default)]
struct FakeInvocations {
    invoked: Vec<String>,
    requested: Vec<String>,
    artifact: bool,
}

impl SkillInvocationState for FakeInvocations {
    fn has_invoked(&self, name: &str) -> bool {
        self.invoked.iter().any(|entry| entry == name)
    }

    fn has_user_requested(&self, name: &str) -> bool {
        self.requested.iter().any(|entry| entry == name)
    }

    fn has_plan_artifact(&self) -> bool {
        self.artifact
    }

    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> {
        if self.artifact {
            vec![PlanArtifactReference {
                path: ".omo/plans/gate-plan.md".into(),
                count: 1,
                last_touched_at: 1,
            }]
        } else {
            Vec::new()
        }
    }
}

/// Test policy deps: optionally wires a skill-invocation resolver and applies
/// the plan gate for plan-gated agents (fails closed when no resolver is wired).
struct FakePolicy {
    resolver: Option<Arc<SkillInvocationResolver>>,
}

impl SpawnPolicyDeps for FakePolicy {
    fn invocation_gate_denial(&self, subagent_type: &str, session_id: &str) -> Option<String> {
        if !plan_gated_agent_names().contains(subagent_type) {
            return None;
        }
        let requirement = format!(
            "Agent \"{subagent_type}\" is plan-gated: the user must request the ulw-plan skill and a plan artifact must exist before spawning it."
        );
        let Some(resolver) = self.resolver.as_ref() else {
            return Some(requirement);
        };
        let invocations = resolver(session_id);
        if invocations.has_invoked("start-work") {
            return Some(format!(
                "Agent \"{subagent_type}\" is plan-gated: it cannot be spawned after start-work was invoked in this session."
            ));
        }
        if invocations.has_user_requested("ulw-plan") && invocations.has_plan_artifact() {
            return None;
        }
        Some(requirement)
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

fn state(invoked: &[&str], requested: &[&str], artifact: bool) -> FakeInvocations {
    FakeInvocations {
        invoked: invoked.iter().map(ToString::to_string).collect(),
        requested: requested.iter().map(ToString::to_string).collect(),
        artifact,
    }
}

fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".into(),
        cwd: "/tmp/senpi-task-gate".into(),
        ..Default::default()
    }
}

fn single(
    prompt: &str,
    subagent_type: Option<&str>,
    category: Option<&str>,
    bg: bool,
) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some(prompt.to_string()),
        subagent_type: subagent_type.map(ToString::to_string),
        category: category.map(ToString::to_string),
        run_in_background: if bg { Some(true) } else { None },
        ..Default::default()
    }
}

fn item(prompt: &str, subagent_type: &str) -> crate::tools::task::validation::SpawnItemInput {
    crate::tools::task::validation::SpawnItemInput {
        prompt: prompt.to_string(),
        subagent_type: Some(subagent_type.to_string()),
        ..Default::default()
    }
}

fn result_text(result: &TaskToolResult) -> String {
    match result.content.first() {
        Some(first) if first.kind == "text" => first.text.clone(),
        _ => String::new(),
    }
}

/// Runs the task tool once; returns the result and how many tasks reached the manager.
fn run(params: SpawnParamsInput, invocations: Option<FakeInvocations>) -> (TaskToolResult, usize) {
    let harness = default_manager();
    let tool = Default::default();
    let policy = FakePolicy {
        resolver: invocations.map(|fake| {
            let resolver: Arc<SkillInvocationResolver> = Arc::new(move |_session_id: &str| {
                let boxed: Box<dyn SkillInvocationState> = Box::new(fake.clone());
                boxed
            });
            resolver
        }),
    };
    let deps = TaskExecuteDeps {
        manager: &harness.manager,
        tool: &tool,
        policy: &policy,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());
    let result = execute
        .execute("c", &params, None, None, &ctx())
        .expect("execute succeeds");
    let count = harness.manager.resident_task_ids().len();
    (result, count)
}

#[test]
fn given_no_resolver_when_spawning_momus_then_fails_closed_without_starting() {
    let (result, count) = run(single("p", Some("momus"), None, false), None);

    assert_eq!(count, 0);
    assert_eq!(result.details.status, "denied");
    assert!(result_text(&result).contains("ulw-plan"));
}

#[test]
fn given_no_invocations_when_spawning_momus_then_denies_naming_ulw_plan() {
    let (result, count) = run(
        single("p", Some("momus"), None, false),
        Some(state(&[], &[], false)),
    );

    assert_eq!(count, 0);
    assert_eq!(result.details.status, "denied");
    assert!(result_text(&result).contains("ulw-plan"));
}

#[test]
fn given_no_invocations_when_spawning_metis_then_denies_naming_ulw_plan() {
    let (result, count) = run(
        single("p", Some("metis"), None, false),
        Some(state(&[], &[], false)),
    );

    assert_eq!(count, 0);
    assert_eq!(result.details.status, "denied");
    assert!(result_text(&result).contains("ulw-plan"));
}

#[test]
fn given_only_skill_md_read_invocation_when_spawning_momus_then_stays_denied() {
    let (result, count) = run(
        single("p", Some("momus"), None, false),
        Some(state(&["ulw-plan"], &[], true)),
    );

    assert_eq!(count, 0);
    assert_eq!(result.details.status, "denied");
}

#[test]
fn given_user_requested_plan_with_artifact_when_spawning_momus_then_gate_opens() {
    let (result, count) = run(
        single("p", Some("momus"), None, true),
        Some(state(&[], &["ulw-plan"], true)),
    );

    assert_eq!(count, 1);
    assert_ne!(result.details.status, "denied");
}

#[test]
fn given_requested_plan_then_start_work_when_spawning_momus_then_denies_naming_start_work() {
    let (result, count) = run(
        single("p", Some("momus"), None, false),
        Some(state(&["start-work"], &["ulw-plan"], true)),
    );

    assert_eq!(count, 0);
    assert_eq!(result.details.status, "denied");
    assert!(result_text(&result).contains("start-work"));
}

#[test]
fn given_only_start_work_when_spawning_momus_then_forbidden_denial_takes_precedence() {
    let (result, count) = run(
        single("p", Some("momus"), None, false),
        Some(state(&["start-work"], &[], false)),
    );

    assert_eq!(count, 0);
    assert_eq!(result.details.status, "denied");
    assert!(result_text(&result).contains("start-work"));
}

#[test]
fn given_no_invocations_when_spawning_explore_then_gate_does_not_apply() {
    let (result, count) = run(
        single("p", Some("explore"), None, true),
        Some(state(&[], &[], false)),
    );

    assert_eq!(count, 1);
    assert_ne!(result.details.status, "denied");
}

#[test]
fn given_no_invocations_when_spawning_category_then_gate_does_not_apply() {
    let (result, count) = run(
        single("p", None, Some("quick"), true),
        Some(state(&[], &[], false)),
    );

    assert_eq!(count, 1);
    assert_ne!(result.details.status, "denied");
}

#[test]
fn given_batch_with_gated_item_when_executed_then_gated_item_fails_and_never_reaches_manager() {
    let params = SpawnParamsInput {
        tasks: Some(vec![
            item("review the plan", "momus"),
            item("scan", "explore"),
        ]),
        run_in_background: Some(true),
        ..Default::default()
    };
    let (result, count) = run(params, Some(state(&[], &[], false)));

    assert_eq!(count, 1);
    let items = result.details.items.clone().unwrap_or_default();
    let gated = items.iter().find(|item| {
        item.error_message
            .as_deref()
            .unwrap_or("")
            .contains("plan-gated")
    });
    let plain = items
        .iter()
        .find(|item| item.subagent_type.as_deref() == Some("explore"));
    let gated = gated.expect("gated item present");
    assert_eq!(gated.status, "error");
    assert!(gated.error_message.as_deref().unwrap_or("").contains("ulw-plan"));
    let plain = plain.expect("plain item present");
    assert_ne!(plain.status, "error");
}
