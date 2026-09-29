//! On-disk layout of team specs and runtime state, with traversal containment.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::logger::{self, TeamCoreLog};
use crate::path_util;
use crate::types::SpecSource;

/// A discovered `config.json` for one team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamSpecEntry {
    pub name: String,
    pub scope: SpecSource,
    pub path: PathBuf,
}

type ModeFn = Box<dyn Fn(&Path, u32) -> io::Result<()>>;
type StatModeFn = Box<dyn Fn(&Path) -> io::Result<u32>>;

/// Injectable filesystem operations for [`ensure_base_dirs_with`].
pub struct PathDeps {
    pub chmod: ModeFn,
    pub log: TeamCoreLog,
    pub mkdir: ModeFn,
    pub stat_mode: StatModeFn,
}

impl Default for PathDeps {
    fn default() -> Self {
        Self {
            chmod: Box::new(|path, mode| {
                fs::set_permissions(path, fs::Permissions::from_mode(mode))
            }),
            log: logger::log_fn(),
            mkdir: Box::new(|path, mode| {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(mode)
                    .create(path)
            }),
            stat_mode: Box::new(|path| {
                fs::metadata(path).map(|metadata| metadata.permissions().mode())
            }),
        }
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

fn get_team_directory(
    base_dir: &Path,
    team_name: &str,
    scope: SpecSource,
    project_root: Option<&Path>,
) -> PathBuf {
    match scope {
        SpecSource::Project => project_root
            .unwrap_or(Path::new(""))
            .join(".omo")
            .join("teams")
            .join(team_name),
        SpecSource::User => base_dir.join("teams").join(team_name),
    }
}

fn resolve_contained_path(base_dir: &Path, segments: &[&str]) -> Result<PathBuf> {
    for segment in segments {
        if segment.is_empty()
            || *segment == "."
            || *segment == ".."
            || segment.contains('/')
            || segment.contains('\\')
            || segment.contains('\0')
        {
            return Err(TeamCoreError::TeamPathTraversal);
        }
    }
    let resolved_base = path_util::resolve(base_dir, &[]);
    let resolved = path_util::resolve(&resolved_base, segments);
    if !resolved.starts_with(&resolved_base) {
        return Err(TeamCoreError::TeamPathTraversal);
    }
    Ok(resolved)
}

fn expand_home_directory(directory: &str) -> PathBuf {
    if directory == "~" {
        return home_dir();
    }
    if let Some(rest) = directory
        .strip_prefix("~/")
        .or_else(|| directory.strip_prefix("~\\"))
    {
        return home_dir().join(rest);
    }
    PathBuf::from(directory)
}

/// `resolveBaseDir`: `base_dir` (with `~` expansion) or `~/.omo`.
#[must_use]
pub fn resolve_base_dir(config: &TeamModeConfig) -> PathBuf {
    match &config.base_dir {
        Some(base_dir) => expand_home_directory(base_dir),
        None => home_dir().join(".maho"),
    }
}

#[must_use]
pub fn get_team_spec_path(
    base_dir: &Path,
    team_name: &str,
    scope: SpecSource,
    project_root: Option<&Path>,
) -> PathBuf {
    get_team_directory(base_dir, team_name, scope, project_root).join("config.json")
}

pub fn get_runtime_state_dir(base_dir: &Path, team_run_id: &str) -> Result<PathBuf> {
    resolve_contained_path(base_dir, &["runtime", team_run_id])
}

pub fn get_inbox_dir(base_dir: &Path, team_run_id: &str, member_name: &str) -> Result<PathBuf> {
    resolve_contained_path(base_dir, &["runtime", team_run_id, "inboxes", member_name])
}

pub fn get_tasks_dir(base_dir: &Path, team_run_id: &str) -> Result<PathBuf> {
    resolve_contained_path(base_dir, &["runtime", team_run_id, "tasks"])
}

fn assert_safe_task_id(task_id: &str) -> Result<()> {
    if task_id.is_empty() || !task_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TeamCoreError::TeamPathTraversal);
    }
    Ok(())
}

pub fn get_task_file_path(base_dir: &Path, team_run_id: &str, task_id: &str) -> Result<PathBuf> {
    assert_safe_task_id(task_id)?;
    resolve_contained_path(
        base_dir,
        &["runtime", team_run_id, "tasks", &format!("{task_id}.json")],
    )
}

pub fn get_task_claims_dir(base_dir: &Path, team_run_id: &str) -> Result<PathBuf> {
    resolve_contained_path(base_dir, &["runtime", team_run_id, "tasks", "claims"])
}

pub fn get_task_claim_lock_path(
    base_dir: &Path,
    team_run_id: &str,
    task_id: &str,
) -> Result<PathBuf> {
    assert_safe_task_id(task_id)?;
    resolve_contained_path(
        base_dir,
        &[
            "runtime",
            team_run_id,
            "tasks",
            "claims",
            &format!("{task_id}.lock"),
        ],
    )
}

pub fn get_worktree_dir(base_dir: &Path, team_run_id: &str, member_name: &str) -> Result<PathBuf> {
    resolve_contained_path(base_dir, &["worktrees", team_run_id, member_name])
}

fn read_team_spec_directories(directory: &Path, scope: SpecSource) -> Vec<TeamSpecEntry> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut specs: Vec<TeamSpecEntry> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            TeamSpecEntry {
                path: path_util::resolve(directory, &[&name, "config.json"]),
                name,
                scope,
            }
        })
        .collect();
    specs.sort_by(|left, right| left.name.cmp(&right.name));
    specs
}

/// `discoverTeamSpecs` with the default logger.
#[must_use]
pub fn discover_team_specs(config: &TeamModeConfig, project_root: &Path) -> Vec<TeamSpecEntry> {
    discover_team_specs_with(config, project_root, &logger::log_fn())
}

/// `discoverTeamSpecs`: project specs first; user specs shadowed by a project spec are logged
/// as a collision and dropped.
#[must_use]
pub fn discover_team_specs_with(
    config: &TeamModeConfig,
    project_root: &Path,
    log: &TeamCoreLog,
) -> Vec<TeamSpecEntry> {
    let base_dir = resolve_base_dir(config);
    let project_teams_dir = path_util::resolve(project_root, &[".omo", "teams"]);
    let user_teams_dir = path_util::resolve(&base_dir, &["teams"]);
    let project_specs = read_team_spec_directories(&project_teams_dir, SpecSource::Project);
    let user_specs = read_team_spec_directories(&user_teams_dir, SpecSource::User);

    let mut discovered = project_specs.clone();
    for user_spec in user_specs {
        if let Some(project_spec) = project_specs
            .iter()
            .find(|entry| entry.name == user_spec.name)
        {
            log(
                "team-spec collision",
                Some(json!({
                    "event": "team-spec-collision",
                    "teamName": user_spec.name,
                    "projectPath": project_spec.path,
                    "userPath": user_spec.path,
                })),
            );
            continue;
        }
        discovered.push(user_spec);
    }
    discovered
}

fn error_code_name(error: &io::Error) -> Option<&'static str> {
    match error.raw_os_error() {
        Some(libc::EPERM) => Some("EPERM"),
        Some(libc::ENOTSUP) => Some("ENOTSUP"),
        Some(libc::EINVAL) => Some("EINVAL"),
        _ => None,
    }
}

fn safe_chmod(directory: &Path, mode: u32, deps: &PathDeps) -> io::Result<()> {
    match (deps.chmod)(directory, mode) {
        Ok(()) => Ok(()),
        Err(error) => match error_code_name(&error) {
            Some(code) => {
                (deps.log)(
                    "team-mode: chmod refused on base directory; continuing with existing permissions",
                    Some(json!({ "path": directory, "code": code, "syscall": "chmod" })),
                );
                Ok(())
            }
            None => Err(error),
        },
    }
}

/// `ensureBaseDirs` with real filesystem operations.
pub fn ensure_base_dirs(base_dir: &Path) -> Result<()> {
    ensure_base_dirs_with(base_dir, &PathDeps::default())
}

/// `ensureBaseDirs`: create base/teams/runtime/worktrees at 0700, tolerating chmod refusal.
pub fn ensure_base_dirs_with(base_dir: &Path, deps: &PathDeps) -> Result<()> {
    let directories = [
        base_dir.to_path_buf(),
        base_dir.join("teams"),
        base_dir.join("runtime"),
        base_dir.join("worktrees"),
    ];
    for directory in &directories {
        (deps.mkdir)(directory, 0o700)?;
        safe_chmod(directory, 0o700, deps)?;
    }
    for directory in &directories {
        if (deps.stat_mode)(directory)? & 0o777 != 0o700 {
            safe_chmod(directory, 0o700, deps)?;
        }
    }
    Ok(())
}
