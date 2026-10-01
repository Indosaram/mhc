//! `tools/task/execute-skills.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::default_manager;
use crate::state::TaskSpawnSpec;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::types::{SkillLoader, SkillResolution, TaskSkillSummary};
use crate::tools::task::validation::SpawnParamsInput;

/// `CTX`: the parent tool context used by the TS fixtures.
fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".to_string(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        ..TaskToolContext::default()
    }
}

/// `makeDeps(manager, { loadSkills })`: tool deps with a scripted skill loader.
fn tool_with_skills(
    load: impl Fn(&[String], &str) -> SkillResolution + Send + Sync + 'static,
) -> TaskToolDeps {
    let loader: Arc<SkillLoader> = Arc::new(load);
    TaskToolDeps {
        load_skills: Some(loader),
        ..TaskToolDeps::default()
    }
}

fn background_params(prompt: &str, skill: &str) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some(prompt.to_string()),
        category: Some("quick".to_string()),
        load_skills: Some(vec![skill.to_string()]),
        run_in_background: Some(true),
        ..SpawnParamsInput::default()
    }
}

#[test]
fn given_load_skills_when_spawning_then_resolved_skill_md_content_is_prepended_to_the_child_prompt()
{
    let harness = default_manager();
    let tool = tool_with_skills(|names, _cwd| SkillResolution {
        prepend: if names.is_empty() {
            String::new()
        } else {
            "SKILL DIRECTIVE\n\n".to_string()
        },
        resolved: names.to_vec(),
        missing: Vec::new(),
        ..SkillResolution::default()
    });
    let deps = TaskExecuteDeps {
        manager: &harness.manager,
        tool: &tool,
        policy: &tool,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let result = execute
        .execute(
            "resolved-skill",
            &background_params("do the thing", "reviewer"),
            None,
            None,
            &ctx(),
        )
        .expect("execute succeeds");

    let record = harness
        .manager
        .get(&result.details.task_id)
        .expect("started task record");
    let prompt = match record.spawn_spec.as_ref().expect("spawn spec") {
        TaskSpawnSpec::V1(v1) => v1.prompt.clone(),
        TaskSpawnSpec::LegacyProcess { .. } => panic!("expected v1 spawn spec"),
    };
    assert!(prompt.starts_with("SKILL DIRECTIVE"));
    assert!(prompt.ends_with("do the thing"));
}

#[test]
fn given_a_missing_requested_skill_when_background_spawn_succeeds_then_the_task_result_reports_it_without_failing()
 {
    let harness = default_manager();
    let tool = tool_with_skills(|names, _cwd| SkillResolution {
        prepend: String::new(),
        resolved: Vec::new(),
        missing: names.to_vec(),
        ..SkillResolution::default()
    });
    let deps = TaskExecuteDeps {
        manager: &harness.manager,
        tool: &tool,
        policy: &tool,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let result = execute
        .execute(
            "missing-skill",
            &background_params("continue anyway", "ghost"),
            None,
            None,
            &ctx(),
        )
        .expect("execute succeeds");

    let text = result
        .content
        .first()
        .filter(|content| content.kind == "text")
        .map(|content| content.text.clone())
        .unwrap_or_default();
    assert_eq!(result.details.status, "running");
    assert_eq!(
        result.details.skills,
        Some(TaskSkillSummary {
            requested: vec!["ghost".to_string()],
            resolved: Vec::new(),
            missing: vec!["ghost".to_string()],
        })
    );
    assert!(text.contains("Missing skills: ghost"));
}
