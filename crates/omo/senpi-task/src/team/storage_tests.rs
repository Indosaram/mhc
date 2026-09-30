//! `team/storage.test.ts`

use std::path::{MAIN_SEPARATOR, Path};

use pretty_assertions::assert_eq;

use crate::store::StateDirConfig;
use crate::team::storage::{
    ensure_team_runtime_dirs, resolve_project_team_spec_path, resolve_team_member_inbox_dir,
    resolve_team_runtime_dirs, team_storage_base_dir,
};

fn config_for(project_dir: &Path) -> StateDirConfig {
    StateDirConfig {
        project_dir: project_dir.to_string_lossy().into_owned().into(),
        ..Default::default()
    }
}

#[test]
fn given_a_project_state_dir_config_when_the_base_dir_is_resolved_then_it_lands_under_the_senpi_task_state_dir() {
    let project = tempfile::tempdir().expect("tempdir");
    let config = config_for(project.path());

    let base_dir = team_storage_base_dir(&config);

    assert_eq!(base_dir, project.path().join(".omo").join("senpi-task").join("teams"));
}

#[test]
fn given_a_team_run_id_when_runtime_dirs_are_resolved_then_runtime_and_tasks_dirs_live_under_the_base_dir() {
    let project = tempfile::tempdir().expect("tempdir");
    let config = config_for(project.path());
    let team_run_id = "11111111-1111-1111-1111-111111111111";

    let dirs = resolve_team_runtime_dirs(&config, team_run_id).expect("dirs");

    assert_eq!(dirs.base_dir, project.path().join(".omo").join("senpi-task").join("teams"));
    assert_eq!(dirs.runtime_dir, dirs.base_dir.join("runtime").join(team_run_id));
    assert_eq!(dirs.tasks_dir, dirs.base_dir.join("runtime").join(team_run_id).join("tasks"));
    assert_eq!(
        resolve_team_member_inbox_dir(&config, team_run_id, "finder").expect("inbox"),
        dirs.base_dir.join("runtime").join(team_run_id).join("inboxes").join("finder")
    );
}

#[test]
fn given_the_discovery_only_project_spec_path_when_resolved_then_it_maps_to_the_omo_compatible_teams_path() {
    let project = tempfile::tempdir().expect("tempdir");

    let spec_path = resolve_project_team_spec_path(project.path(), "research-team");

    assert_eq!(
        spec_path,
        project.path().join(".omo").join("teams").join("research-team").join("config.json")
    );
    let marker = format!("{MAIN_SEPARATOR}senpi-task{MAIN_SEPARATOR}");
    assert!(!spec_path.to_string_lossy().contains(&marker));
}

#[test]
fn given_a_team_run_when_runtime_dirs_are_ensured_then_the_directories_are_created_at_the_right_paths() {
    let project = tempfile::tempdir().expect("tempdir");
    let config = config_for(project.path());
    let team_run_id = "22222222-2222-2222-2222-222222222222";

    let dirs = ensure_team_runtime_dirs(&config, team_run_id, &["finder", "quick-1"]).expect("ensure");

    assert!(dirs.runtime_dir.exists());
    assert!(dirs.tasks_dir.exists());
    assert!(resolve_team_member_inbox_dir(&config, team_run_id, "finder").expect("inbox").exists());
    assert!(resolve_team_member_inbox_dir(&config, team_run_id, "quick-1").expect("inbox").exists());
}
