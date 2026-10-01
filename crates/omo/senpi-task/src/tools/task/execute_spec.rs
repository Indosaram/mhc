//! `tools/task/execute-spec.ts`: builds the manager start spec for one resolved spawn.

use std::collections::HashMap;
use std::sync::Arc;

use crate::manager::execution_mode::{ExecutionMode, ExecutionModeSources, resolve_execution_mode};
use crate::manager::types::ManagerStartSpec;
use crate::tools::task::skill_result::task_skill_summary;
use crate::tools::task::skills::{FsSkillLoaderOptions, create_fs_skill_loader};
use crate::tools::task::spawn_policy::SpawnPolicyDeps;
use crate::tools::task::types::{ResolvedSpawnItem, SkillLoader, SpawnTarget, TaskSkillSummary};

/// Session ancestry reported by the host for a parent session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskAncestry {
    pub root_session_id: String,
    pub depth: u64,
}

pub type ResolveAncestry = Arc<dyn Fn(&str) -> Option<TaskAncestry> + Send + Sync>;

/// The slice of a host agent definition the task tool reads (`executionMode`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskAgentDefinition {
    pub execution_mode: Option<String>,
}

/// `omoConfig.agents.<name>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskOmoAgentConfig {
    pub execution_mode: Option<ExecutionMode>,
}

/// `omoConfig.task`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskOmoTaskConfig {
    pub default_execution_mode: Option<ExecutionMode>,
}

/// The slice of omo.json the task tool reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskOmoConfig {
    pub agents: HashMap<String, TaskOmoAgentConfig>,
    pub task: Option<TaskOmoTaskConfig>,
}

/// Task tool dependencies used by spec building.
#[derive(Clone, Default)]
pub struct TaskToolDeps {
    pub resolve_ancestry: Option<ResolveAncestry>,
    pub load_skills: Option<Arc<SkillLoader>>,
    pub agents: HashMap<String, TaskAgentDefinition>,
    pub omo_config: TaskOmoConfig,
}

/// The TS `TaskToolDeps` carries no spawn-policy seams of its own: `invocationGateDenial` and
/// `planReviewContractOutcome` are consulted only when the host wires the composed policy deps
/// separately, so the default deps object never denies or forces a spawn.
impl SpawnPolicyDeps for TaskToolDeps {
    fn invocation_gate_denial(&self, _subagent_type: &str, _session_id: &str) -> Option<String> {
        None
    }

    fn plan_review_contract_outcome(
        &self,
        _subagent_type: &str,
        _caller_prompt: &str,
        _session_id: &str,
    ) -> Option<crate::tools::task::spawn_policy::PlanReviewContractOutcome> {
        None
    }
}

/// Task tool params for a single spawn (prompt required, no `tasks`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SingleSpawnParams {
    pub prompt: String,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub run_in_background: Option<bool>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub load_skills: Option<Vec<String>>,
}

/// `ManagerStartSpec & { execution_mode, skills? }`.
pub struct ResolvedManagerStartSpec {
    pub spec: ManagerStartSpec,
    pub execution_mode: ExecutionMode,
    pub skills: Option<TaskSkillSummary>,
}

pub fn build_start_spec(
    params: &SingleSpawnParams,
    target: &SpawnTarget,
    parent_session_id: &str,
    deps: &TaskToolDeps,
    cwd: &str,
) -> ResolvedManagerStartSpec {
    let ancestry = deps
        .resolve_ancestry
        .as_ref()
        .and_then(|resolve| resolve(parent_session_id));
    let load_skills = deps
        .load_skills
        .clone()
        .unwrap_or_else(|| create_fs_skill_loader(FsSkillLoaderOptions::default()));
    let requested = params.load_skills.clone().unwrap_or_default();
    let skills = load_skills(&requested, cwd);
    let skill_summary = task_skill_summary(&requested, &skills);
    let execution_mode = resolved_task_execution_mode(target, deps);
    let depth: u64 = ancestry.as_ref().map_or(0, |ancestry| ancestry.depth) + 1;
    let root_session_id = ancestry.map_or_else(
        || parent_session_id.to_string(),
        |ancestry| ancestry.root_session_id,
    );
    let (category, subagent_type) = match target {
        SpawnTarget::Category(category) => (Some(category.clone()), None),
        SpawnTarget::SubagentType(agent) => (None, Some(agent.clone())),
    };
    let spec = ManagerStartSpec {
        prompt: format!("{}{}", skills.prepend, params.prompt),
        task_summary: params.task_summary.clone(),
        parent_session_id: parent_session_id.to_string(),
        root_session_id: Some(root_session_id),
        depth: TryFrom::try_from(depth).unwrap_or_default(),
        category,
        subagent_type,
        model: params.model.clone(),
        name: params.name.clone(),
        description: params.description.clone(),
        run_in_background: params.run_in_background.unwrap_or(false),
        ..Default::default()
    };
    ResolvedManagerStartSpec {
        spec,
        execution_mode,
        skills: skill_summary,
    }
}

/// Only `"in-process"` and `"process"` are execution modes.
fn to_execution_mode(value: Option<&str>) -> Option<ExecutionMode> {
    match value {
        Some(mode @ ("in-process" | "process")) => ExecutionMode::parse(mode),
        _ => None,
    }
}

fn resolved_agent_mode(target: &SpawnTarget, deps: &TaskToolDeps) -> Option<ExecutionMode> {
    let SpawnTarget::SubagentType(agent) = target else {
        return None;
    };
    to_execution_mode(
        deps.agents
            .get(agent)
            .and_then(|definition| definition.execution_mode.as_deref()),
    )
    .or_else(|| {
        deps.omo_config
            .agents
            .get(agent)
            .and_then(|config| config.execution_mode)
    })
}

fn resolved_task_execution_mode(target: &SpawnTarget, deps: &TaskToolDeps) -> ExecutionMode {
    let agent_mode = resolved_agent_mode(target, deps);
    resolve_execution_mode(ExecutionModeSources {
        spec_mode: None,
        agent_mode,
        config_mode: deps
            .omo_config
            .task
            .as_ref()
            .and_then(|task| task.default_execution_mode),
    })
}

pub fn single_spawn_params(
    item: &ResolvedSpawnItem,
    run_in_background: Option<bool>,
) -> SingleSpawnParams {
    let (category, subagent_type) = match &item.target {
        SpawnTarget::Category(category) => (Some(category.clone()), None),
        SpawnTarget::SubagentType(agent) => (None, Some(agent.clone())),
    };
    SingleSpawnParams {
        prompt: item.prompt.clone(),
        task_summary: item.task_summary.clone(),
        description: item.description.clone(),
        category,
        subagent_type,
        run_in_background,
        name: item.name.clone(),
        model: item.model.clone(),
        load_skills: Some(item.load_skills.clone()),
    }
}
