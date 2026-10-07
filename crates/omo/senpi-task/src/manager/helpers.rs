//! Pure record/spec builders (`manager/manager-helpers.ts`).

use std::path::Path;

use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{ManagedStartSpec, ManagerStartSpec, ResolvedChildPlan};
use crate::state::{SpawnSpecV1, TaskRecord, TaskRecordInput};

pub const MEMBER_TASK_ID_ENV: &str = "SENPI_TASK_MEMBER_TASK_ID";

pub fn now_iso(now_ms: i64) -> String {
    crate::shared::iso_from_ms(now_ms)
}

pub fn child_state_dir(state_dir: &Path, task_id: &str) -> String {
    state_dir
        .join("children")
        .join(task_id)
        .to_string_lossy()
        .into_owned()
}

pub fn build_record_input(
    spec: &ManagerStartSpec,
    plan: &ResolvedChildPlan,
    name: &str,
    execution_mode: ExecutionMode,
) -> TaskRecordInput {
    TaskRecordInput {
        name: Some(name.to_string()),
        parent_session_id: spec.parent_session_id.clone(),
        root_session_id: spec
            .root_session_id
            .clone()
            .unwrap_or_else(|| spec.parent_session_id.clone()),
        depth: spec.depth,
        execution_mode: execution_mode.as_str().to_string(),
        model: plan.model.clone(),
        notify_on_terminal: spec.run_in_background,
        task_summary: spec.task_summary.clone(),
        description: spec.description.clone(),
        requested_model: plan.requested_model.clone(),
        fallback_models: plan.fallback_models.clone(),
        resolved_model: plan.resolved_model.clone(),
        agent_type: spec.subagent_type.clone().or(plan.agent_type.clone()),
        category: spec.category.clone().or(plan.category.clone()),
        tool_allow: plan.tool_allowlist.clone(),
        tool_deny: plan.tool_denylist.clone(),
        ..TaskRecordInput::default()
    }
}

pub fn build_managed_spec(
    record: &TaskRecord,
    spec: &ManagerStartSpec,
    plan: &ResolvedChildPlan,
    cwd: &str,
    state_dir: &Path,
) -> ManagedStartSpec {
    let prompt = match plan.prompt_append.as_deref() {
        Some(append) if !append.is_empty() => format!("{}\n\n{append}", spec.prompt),
        _ => spec.prompt.clone(),
    };
    let member_env = spec.member_env.as_ref().map(|env| {
        let mut env = env.clone();
        env.insert(MEMBER_TASK_ID_ENV.to_string(), record.task_id.clone());
        env
    });
    ManagedStartSpec {
        task_id: record.task_id.clone(),
        cwd: spec.cwd.clone().unwrap_or_else(|| cwd.to_string()),
        state_dir: child_state_dir(state_dir, &record.task_id),
        prompt,
        depth: spec.depth,
        parent_session_id: spec.parent_session_id.clone(),
        root_session_id: spec
            .root_session_id
            .clone()
            .unwrap_or_else(|| spec.parent_session_id.clone()),
        model: Some(plan.model.clone()),
        requested_model: plan.requested_model.clone(),
        fallback_models: plan.fallback_models.clone(),
        resolved_model: plan.resolved_model.clone(),
        variant: plan.variant.clone(),
        agent_type: record.agent_type.clone(),
        instructions: spec.instructions.clone().or(plan.instructions.clone()),
        tool_allowlist: plan.tool_allowlist.clone(),
        tool_denylist: record.tool_deny.clone(),
        member_scoped_tool_names: spec
            .member_scoped_tools
            .as_ref()
            .map(|tools| tools.iter().map(|tool| tool.name().to_string()).collect()),
        member_scoped_tools: spec.member_scoped_tools.clone(),
        extensions: spec.extensions.clone(),
        member_env,
    }
}

pub fn build_spawn_spec_v1(spec: &ManagedStartSpec) -> SpawnSpecV1 {
    SpawnSpecV1 {
        cwd: spec.cwd.clone(),
        prompt: spec.prompt.clone(),
        instructions: spec.instructions.clone(),
        member_scoped_tool_names: spec.member_scoped_tool_names.clone(),
        isolation: None,
    }
}

pub const SPAWN_SPEC_UNAVAILABLE_REASON: &str =
    "record has no persisted v1 spawn_spec to rebuild from";

/// `Err` carries the `spawn_spec_unavailable` reason.
pub fn build_respawn_managed_spec(
    record: &TaskRecord,
    state_dir: &Path,
) -> Result<ManagedStartSpec, &'static str> {
    let Some(spawn_spec) = record.spawn_spec.as_ref().and_then(|spec| spec.as_v1()) else {
        return Err(SPAWN_SPEC_UNAVAILABLE_REASON);
    };
    Ok(ManagedStartSpec {
        task_id: record.task_id.clone(),
        cwd: spawn_spec.cwd.clone(),
        state_dir: child_state_dir(state_dir, &record.task_id),
        prompt: spawn_spec.prompt.clone(),
        depth: record.depth,
        parent_session_id: record.parent_session_id.clone(),
        root_session_id: record.root_session_id.clone(),
        model: Some(record.model.clone()),
        requested_model: record.requested_model.clone(),
        fallback_models: record.fallback_models.clone(),
        resolved_model: record.resolved_model.clone(),
        variant: record
            .resolved_model
            .as_ref()
            .and_then(|model| model.variant.clone()),
        agent_type: record.agent_type.clone(),
        instructions: spawn_spec.instructions.clone(),
        tool_allowlist: record.tool_allow.clone(),
        tool_denylist: record.tool_deny.clone(),
        member_scoped_tool_names: spawn_spec.member_scoped_tool_names.clone(),
        ..ManagedStartSpec::default()
    })
}

pub fn in_session(record: &TaskRecord, session_id: &str) -> bool {
    record.parent_session_id == session_id || record.root_session_id == session_id
}

pub fn record_spawned_pid(record: &TaskRecord, pid: Option<i64>) -> Option<TaskRecord> {
    let pid = pid?;
    if is_terminal_record(record) {
        return None;
    }
    Some(TaskRecord {
        pid: Some(pid),
        ..record.clone()
    })
}

pub fn is_terminal_record(record: &TaskRecord) -> bool {
    record.status.is_terminal()
}
