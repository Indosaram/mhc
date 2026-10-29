//! `team/member-respawn.test.ts`

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::team_state_store::load_runtime_state;
use tempfile::TempDir;

use crate::store::StateDirConfig;
use crate::team::member_respawn::{
    TeamMemberRespawnLaunchErrorCode, TeamMemberRespawnLaunchResolverOptions,
    create_team_member_respawn_launch_resolver,
};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::create_team;
use crate::team::runtime_config::{TeamTaskBounds, to_team_core_config};
use crate::team::runtime_fakes::{FakeTeamManager, FakeTeamManagerOptions, TeamBoundsOverrides, team_bounds};
use crate::team::runtime_types::{
    CreateTeamDeps, CreateTeamResult, TeamMemberExtensionConfig, TeamMemberReadPort, TeamMemberTaskRecord,
    TeamRuntimeManagerPort,
};
use crate::team::shutdown::{
    ApproveShutdownDeps, RequestShutdownDeps, ShutdownOutboundMessage, approve_shutdown, request_shutdown,
};
use crate::team::storage::team_storage_base_dir;

const TRUSTED_ENTRY: &str = "/trusted/member-extension.js";
const TRUSTED_PROVIDER: &str = "/trusted/provider-extension.js";

fn state_dir_config(project: &TempDir) -> StateDirConfig {
    StateDirConfig {
        project_dir: project.path().to_string_lossy().into_owned().into(),
        ..Default::default()
    }
}

fn member_extension(inherited: Option<Vec<String>>) -> TeamMemberExtensionConfig {
    TeamMemberExtensionConfig {
        entry_path: TRUSTED_ENTRY.to_string(),
        inherited_extensions: inherited,
    }
}

struct Created {
    project: TempDir,
    bounds: TeamTaskBounds,
    created: CreateTeamResult,
    task_id: String,
    record: TeamMemberTaskRecord,
}

fn create_squad(inherited: Option<Vec<String>>) -> Created {
    let project = tempfile::tempdir().expect("tempdir");
    let bounds = team_bounds(TeamBoundsOverrides::default());
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let spec = normalize_senpi_team_spec(
        &json!({ "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "task alpha" }] }),
        "squad",
        None,
    )
    .expect("normalize spec");
    let manager_port: Arc<dyn TeamRuntimeManagerPort> = manager.clone();
    let created = create_team(
        &spec,
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: manager_port,
            state_dir: state_dir_config(&project),
            team_bounds: bounds,
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: Some(member_extension(inherited)),
            write_member_map: None,
        },
    )
    .expect("create team");
    let task_id = created
        .member_task_ids
        .get("alpha")
        .cloned()
        .expect("expected alpha task id");
    let record = TeamMemberReadPort::get(manager.as_ref(), &task_id).expect("expected team member record");
    Created {
        project,
        bounds,
        created,
        task_id,
        record,
    }
}

#[test]
fn given_persisted_team_member_task_when_resolved_against_current_team_runtime_then_trusted_extension_and_identity_replace_persisted_launch_inputs()
 {
    // given
    let inherited = vec![TRUSTED_PROVIDER.to_string()];
    let fixture = create_squad(Some(inherited.clone()));
    let resolver = create_team_member_respawn_launch_resolver(TeamMemberRespawnLaunchResolverOptions {
        state_dir: state_dir_config(&fixture.project),
        team_bounds: fixture.bounds,
        member_extension: member_extension(Some(inherited)),
    })
    .expect("resolver");

    // when
    let launch = resolver
        .resolve(fixture.record.name.as_deref(), &fixture.record.task_id)
        .expect("launch");

    // then
    assert_eq!(
        launch.extensions,
        vec![TRUSTED_ENTRY.to_string(), TRUSTED_PROVIDER.to_string()]
    );
    let env = launch.member_env.expect("member env");
    assert_eq!(
        env.get("SENPI_TASK_MEMBER").cloned(),
        Some(format!("{}::alpha", fixture.created.runtime_state.team_run_id))
    );
    assert_eq!(
        env.get("SENPI_TASK_MEMBER_TASK_ID").cloned(),
        Some(fixture.task_id.clone())
    );
    let team_config = env.get("SENPI_TASK_TEAM_CONFIG").expect("team config");
    assert!(!team_config.contains("untrusted"));
}

#[test]
fn given_team_member_task_missing_from_current_task_map_when_resolved_then_reattach_is_rejected() {
    // given
    let fixture = create_squad(None);
    let resolver = create_team_member_respawn_launch_resolver(TeamMemberRespawnLaunchResolverOptions {
        state_dir: state_dir_config(&fixture.project),
        team_bounds: fixture.bounds,
        member_extension: member_extension(None),
    })
    .expect("resolver");

    // when
    let error = resolver
        .resolve(fixture.record.name.as_deref(), "st_999999")
        .expect_err("expected rejection");

    // then
    assert!(error.to_string().contains("task_mapping_mismatch"));
    assert_eq!(error.code, TeamMemberRespawnLaunchErrorCode::TaskMappingMismatch);
}

#[test]
fn given_shutdown_approved_team_member_when_resolved_then_reattach_is_rejected_as_member_missing_and_no_launch_is_produced()
 {
    // given
    let fixture = create_squad(None);
    let team_run_id = fixture.created.runtime_state.team_run_id.clone();
    let base_dir = team_storage_base_dir(&state_dir_config(&fixture.project))
        .to_string_lossy()
        .into_owned();
    let config = to_team_core_config(&fixture.bounds, &base_dir).expect("config");
    let sent: Mutex<Vec<ShutdownOutboundMessage>> = Mutex::new(Vec::new());
    let send_message = |message: &ShutdownOutboundMessage| -> Result<(), String> {
        sent.lock().expect("sent lock").push(message.clone());
        Ok(())
    };
    let cancelled: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let cancel_member_task = |member_name: &str| -> Result<(), String> {
        cancelled.lock().expect("cancelled lock").push(member_name.to_string());
        Ok(())
    };
    request_shutdown(
        &team_run_id,
        "alpha",
        &RequestShutdownDeps {
            config: &config,
            send_message: &send_message,
            now: None,
        },
    )
    .expect("request shutdown");
    approve_shutdown(
        &team_run_id,
        "alpha",
        &ApproveShutdownDeps {
            config: &config,
            send_message: &send_message,
            now: None,
            cancel_member_task: &cancel_member_task,
        },
    )
    .expect("approve shutdown");
    let runtime = load_runtime_state(&team_run_id, &config).expect("runtime state");
    let alpha = runtime
        .members
        .iter()
        .find(|member| member.name == "alpha")
        .expect("alpha member");
    assert_eq!(
        serde_json::to_value(alpha.status).expect("status json"),
        json!("shutdown_approved")
    );
    let resolver = create_team_member_respawn_launch_resolver(TeamMemberRespawnLaunchResolverOptions {
        state_dir: state_dir_config(&fixture.project),
        team_bounds: fixture.bounds,
        member_extension: member_extension(None),
    })
    .expect("resolver");

    // when
    let attempt = resolver.resolve(fixture.record.name.as_deref(), &fixture.record.task_id);

    // then
    let error = attempt.expect_err("expected rejection");
    assert_eq!(error.name(), "TeamMemberRespawnLaunchError");
    assert_eq!(error.code, TeamMemberRespawnLaunchErrorCode::MemberMissing);
    assert!(error.to_string().contains("member_missing"));
}
