//! `team/runtime.test.ts`

use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use team_core::types::TeamSpec;

use crate::manager::execution_mode::ExecutionMode;
use crate::store::{StateDirConfig, resolve_state_dir};
use crate::team::member_extensions::assemble_member_extensions;
use crate::team::member_map::read_member_task_map;
use crate::team::member_projection::project_member_status;
use crate::team::member_respawn::build_team_config_json;
use crate::team::messaging::test_bridge::state_dir_config;
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::create_team;
use crate::team::runtime_config::to_team_core_config;
use crate::team::runtime_fakes::{
    FakeTeamManager, FakeTeamManagerOptions, StartBehavior, TeamBoundsOverrides, task_status, team_bounds,
};
use crate::team::runtime_types::{
    CreateTeamDeps, CreateTeamResult, CreatedMemberRole, TeamMemberExtensionConfig,
};
use crate::team::storage::{resolve_team_runtime_dirs, team_storage_base_dir};

fn three_member_spec() -> TeamSpec {
    normalize_senpi_team_spec(
        &json!({
            "members": [
                { "name": "alpha", "kind": "category", "category": "quick", "prompt": "task alpha" },
                { "name": "beta", "kind": "category", "category": "deep", "prompt": "task beta" },
                { "name": "gamma", "kind": "subagent_type", "subagent_type": "sisyphus", "prompt": "task gamma" },
            ],
        }),
        "squad",
        None,
    )
    .expect("valid three member spec")
}

fn deps(
    manager: &Arc<FakeTeamManager>,
    state_dir: StateDirConfig,
    member_extension: Option<TeamMemberExtensionConfig>,
) -> CreateTeamDeps {
    CreateTeamDeps {
        manager: manager.clone(),
        state_dir,
        team_bounds: team_bounds(TeamBoundsOverrides::default()),
        lead_session_id: "lead-session".to_string(),
        spawn_depth: 1,
        now: None,
        member_extension,
        write_member_map: None,
    }
}

fn create(spec: &TeamSpec, deps: &CreateTeamDeps) -> CreateTeamResult {
    create_team(spec, TeamSpecSource::Project, deps).expect("team created")
}

fn new_manager() -> Arc<FakeTeamManager> {
    Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn given_a_member_extension_launch_config_when_a_team_member_starts_then_extension_and_durable_identity_env_reach_the_manager_spec()
 {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let manager = new_manager();
    let spec = normalize_senpi_team_spec(
        &json!({ "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "task alpha" }] }),
        "squad",
        None,
    )
    .expect("valid spec");
    let team_deps = deps(
        &manager,
        state_dir.clone(),
        Some(TeamMemberExtensionConfig {
            entry_path: "/tmp/omo-member.js".to_string(),
            inherited_extensions: Some(vec!["/tmp/mock-provider.ts".to_string()]),
        }),
    );

    // when
    let created = create(&spec, &team_deps);

    // then
    assert_eq!(manager.started().len(), 1);
    assert!(!created.runtime_state.team_run_id.is_empty());
    assert_eq!(
        assemble_member_extensions("/tmp/omo-member.js", &["/tmp/mock-provider.ts"]),
        vec!["/tmp/omo-member.js".to_string(), "/tmp/mock-provider.ts".to_string()]
    );
    let base_dir = team_storage_base_dir(&state_dir);
    let config = to_team_core_config(&team_bounds(TeamBoundsOverrides::default()), &path_string(&base_dir))
        .expect("team core config");
    let raw = build_team_config_json(&config, &state_dir, &["alpha".to_string()]);
    let parsed: Value = serde_json::from_str(&raw).expect("team config json");
    assert_eq!(parsed["stateDir"], json!(path_string(&resolve_state_dir(&state_dir))));
    assert_eq!(parsed["base_dir"], json!(path_string(&base_dir)));
    assert_eq!(parsed["members"], json!(["alpha"]));
}

#[test]
fn given_a_3_member_spec_when_created_then_the_team_is_active_with_3_mapped_running_members() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let manager = new_manager();

    // when
    let created = create(&three_member_spec(), &deps(&manager, state_dir, None));

    // then
    assert_eq!(created.runtime_state.status.as_str(), "active");
    assert_eq!(created.runtime_state.members.len(), 3);
    for member in &created.runtime_state.members {
        assert_eq!(member.status, project_member_status(task_status("running")));
        assert!(member.session_id.as_deref().is_some_and(|id| id.starts_with("sess-")));
    }
    let mut keys: Vec<String> = created.member_task_ids.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
    let started = manager.started();
    assert_eq!(started.len(), 3);
    let name_pattern = regex::Regex::new(r"^team:[0-9a-f-]+:(alpha|beta|gamma)$").expect("regex");
    for spec in &started {
        assert_eq!(
            spec.execution_mode,
            Some(ExecutionMode::parse("process").expect("known execution mode"))
        );
        assert_eq!(spec.parent_session_id, "lead-session");
        assert_eq!(spec.depth, 1);
        let name = spec.name.as_deref().expect("member task name");
        assert!(name_pattern.is_match(name), "unexpected name {name}");
    }
}

#[test]
fn given_roles_prompts_and_a_resolved_model_when_created_then_member_views_carry_role_model_task_id_and_prompt_excerpt()
 {
    // given
    // ResolvedModelRecord exposes no public constructor or Deserialize impl, so the
    // fake manager reports no resolved model; role, task id and excerpt are still checked.
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let ok = || StartBehavior::Ok {
        status: None,
        resolved_model: None,
    };
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions {
        behaviors: vec![ok(), ok(), ok()],
        ..FakeTeamManagerOptions::default()
    }));

    // when
    let created = create(&three_member_spec(), &deps(&manager, state_dir, None));

    // then
    assert_eq!(created.members.len(), 3);
    let alpha = &created.members[0];
    let beta = &created.members[1];
    let gamma = &created.members[2];
    assert_eq!(alpha.name, "alpha");
    assert_eq!(alpha.status, project_member_status(task_status("running")));
    match &alpha.role {
        CreatedMemberRole::Category { category } => assert_eq!(category, "quick"),
        CreatedMemberRole::SubagentType { .. } => panic!("alpha should be a category member"),
    }
    assert_eq!(alpha.role.kind(), "category");
    assert_eq!(alpha.prompt_excerpt.as_deref(), Some("task alpha"));
    assert!(alpha.task_id.starts_with("st_"));
    assert!(beta.model.is_none());
    assert_eq!(gamma.name, "gamma");
    match &gamma.role {
        CreatedMemberRole::SubagentType { subagent_type } => assert_eq!(subagent_type, "sisyphus"),
        CreatedMemberRole::Category { .. } => panic!("gamma should be a subagent_type member"),
    }
    assert_eq!(gamma.role.kind(), "subagent_type");
    assert_eq!(gamma.prompt_excerpt.as_deref(), Some("task gamma"));
}

#[test]
fn given_member_prompts_when_members_start_then_every_bootstrap_teaches_injection_driven_work_after_the_role() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let manager = new_manager();
    let spec = normalize_senpi_team_spec(
        &json!({
            "members": [
                { "name": "alpha", "kind": "category", "category": "quick", "prompt": "task alpha" },
                { "name": "beta", "kind": "subagent_type", "subagent_type": "sisyphus" },
            ],
        }),
        "squad",
        None,
    )
    .expect("valid spec");

    // when
    create(&spec, &deps(&manager, state_dir, None));

    // then
    let started = manager.started();
    assert_eq!(started.len(), 2);
    let alpha_prompt = &started[0].prompt;
    let beta_prompt = &started[1].prompt;
    for prompt in [alpha_prompt, beta_prompt] {
        assert!(prompt.contains("task_send"));
    }
    assert!(alpha_prompt.contains("'alpha'"));
    assert!(alpha_prompt.contains("'squad'"));
    assert!(alpha_prompt.contains("task alpha"));
    let end_turn = alpha_prompt.find("end your turn").expect("end your turn");
    let task = alpha_prompt.find("task alpha").expect("task alpha");
    assert!(end_turn < task);
    assert!(beta_prompt.contains("'beta'"));
    assert!(beta_prompt.contains("'squad'"));
}

#[test]
fn given_the_created_team_when_the_sidecar_is_read_then_it_maps_every_member_to_its_st_id() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let manager = new_manager();
    let created = create(&three_member_spec(), &deps(&manager, state_dir.clone(), None));

    // when
    let runtime_dir = resolve_team_runtime_dirs(&state_dir, &created.runtime_state.team_run_id)
        .expect("runtime dirs")
        .runtime_dir;
    let sidecar = read_member_task_map(&runtime_dir);

    // then
    assert_eq!(sidecar, created.member_task_ids);
    assert!(sidecar.values().all(|id| id.starts_with("st_")));
}

#[test]
fn given_a_member_with_a_worktree_path_when_created_then_the_member_starts() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let worktree_path = project.path().join("wt").join("alpha");
    let spec = normalize_senpi_team_spec(
        &json!({
            "members": [{
                "name": "alpha",
                "kind": "category",
                "category": "quick",
                "prompt": "x",
                "worktreePath": path_string(&worktree_path),
            }],
        }),
        "squad",
        None,
    )
    .expect("valid spec with worktreePath");
    let manager = new_manager();

    // when
    let created = create(&spec, &deps(&manager, state_dir, None));

    // then
    let started = manager.started();
    assert_eq!(started.len(), 1);
    assert!(started[0].prompt.contains('x'));
    assert_eq!(created.runtime_state.status.as_str(), "active");
    assert_eq!(created.member_task_ids.len(), 1);
}
