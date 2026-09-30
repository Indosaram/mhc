//! Senpi team storage layout on top of the team-core path helpers.

use std::fs::DirBuilder;
use std::io;
use std::path::{Path, PathBuf};

use team_core::TeamCoreError;
use team_core::team_registry::{get_inbox_dir, get_runtime_state_dir, get_tasks_dir};

use crate::store::{StateDirConfig, resolve_state_dir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRuntimeDirs {
    pub base_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub tasks_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum TeamStorageError {
    #[error(transparent)]
    Path(#[from] TeamCoreError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Runtime-state base dir for ALL senpi team storage: `<resolveStateDir(cfg)>/teams`. This is the
/// baseDir passed to the team-core path helpers, NOT the omo-compatible `.omo/teams` discovery path.
pub fn team_storage_base_dir(config: &StateDirConfig) -> PathBuf {
    resolve_state_dir(config).join("teams")
}

pub fn resolve_team_runtime_dirs(config: &StateDirConfig, team_run_id: &str) -> team_core::Result<TeamRuntimeDirs> {
    let base_dir = team_storage_base_dir(config);
    let runtime_dir = get_runtime_state_dir(&base_dir, team_run_id)?;
    let tasks_dir = get_tasks_dir(&base_dir, team_run_id)?;
    Ok(TeamRuntimeDirs {
        base_dir,
        runtime_dir,
        tasks_dir,
    })
}

pub fn resolve_team_member_inbox_dir(
    config: &StateDirConfig,
    team_run_id: &str,
    member_name: &str,
) -> team_core::Result<PathBuf> {
    get_inbox_dir(&team_storage_base_dir(config), team_run_id, member_name)
}

/// The omo-compatible `<projectRoot>/.omo/teams/<name>/config.json` path. Discovery-only: it is read
/// for spec discovery and is NEVER a runtime-write target (runtime state lives under the state dir).
pub fn resolve_project_team_spec_path(project_root: &Path, team_name: &str) -> PathBuf {
    project_root.join(".omo").join("teams").join(team_name).join("config.json")
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub fn ensure_team_runtime_dirs<S: AsRef<str>>(
    config: &StateDirConfig,
    team_run_id: &str,
    member_names: &[S],
) -> Result<TeamRuntimeDirs, TeamStorageError> {
    let dirs = resolve_team_runtime_dirs(config, team_run_id)?;
    create_private_dir(&dirs.runtime_dir)?;
    create_private_dir(&dirs.tasks_dir)?;
    for member_name in member_names {
        let inbox = resolve_team_member_inbox_dir(config, team_run_id, member_name.as_ref())?;
        create_private_dir(&inbox)?;
    }
    Ok(dirs)
}
