//! Trusted respawn launch resolution for team member tasks.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};
use team_core::team_state_store::load_runtime_state;

use crate::manager::helpers::MEMBER_TASK_ID_ENV;
use crate::store::{StateDirConfig, resolve_state_dir};
use crate::team::member_extensions::assemble_member_extensions;
use crate::team::member_map::read_member_task_map;
use crate::team::runtime_config::{TeamCoreConfig, TeamTaskBounds, to_team_core_config};
use crate::team::runtime_types::TeamMemberExtensionConfig;
use crate::team::storage::{resolve_team_runtime_dirs, team_storage_base_dir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMemberTaskIdentity {
    pub team_run_id: String,
    pub member_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamMemberRespawnLaunchErrorCode {
    RuntimeUnavailable,
    RuntimeInactive,
    MemberMissing,
    TaskMappingMismatch,
}

impl TeamMemberRespawnLaunchErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RuntimeUnavailable => "runtime_unavailable",
            Self::RuntimeInactive => "runtime_inactive",
            Self::MemberMissing => "member_missing",
            Self::TaskMappingMismatch => "task_mapping_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TeamMemberRespawnLaunchError {
    pub message: String,
    pub code: TeamMemberRespawnLaunchErrorCode,
}

impl TeamMemberRespawnLaunchError {
    pub fn new(code: TeamMemberRespawnLaunchErrorCode, identity: &TeamMemberTaskIdentity) -> Self {
        Self {
            message: format!("Cannot respawn team member '{}': {}", identity.member_name, code.as_str()),
            code,
        }
    }

    pub fn name(&self) -> &'static str {
        "TeamMemberRespawnLaunchError"
    }
}

pub struct TeamMemberRespawnLaunchResolverOptions {
    pub state_dir: StateDirConfig,
    pub team_bounds: TeamTaskBounds,
    pub member_extension: TeamMemberExtensionConfig,
}

/// The trusted launch a respawned task receives: its extension list and, for team members, the
/// member env block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMemberRespawnLaunch {
    pub extensions: Vec<String>,
    pub member_env: Option<BTreeMap<String, String>>,
}

pub struct TeamMemberRespawnLaunchResolver {
    config: TeamCoreConfig,
    state_dir: StateDirConfig,
    inherited_extensions: Vec<String>,
    extensions: Vec<String>,
}

fn is_team_run_id_char(ch: char) -> bool {
    ch.is_ascii_digit() || ('a'..='f').contains(&ch) || ch == '-'
}

fn is_member_name_char(ch: char) -> bool {
    ch.is_ascii_digit() || ch.is_ascii_lowercase() || ch == '-'
}

/// Matches `^team:([0-9a-f-]{36}):([a-z0-9-]+)$`.
pub fn parse_team_member_task_name(name: Option<&str>) -> Option<TeamMemberTaskIdentity> {
    let rest = name?.strip_prefix("team:")?;
    let (team_run_id, member_name) = rest.split_once(':')?;
    let id_ok = team_run_id.chars().count() == 36 && team_run_id.chars().all(is_team_run_id_char);
    let member_ok = !member_name.is_empty() && member_name.chars().all(is_member_name_char);
    (id_ok && member_ok).then(|| TeamMemberTaskIdentity {
        team_run_id: team_run_id.to_string(),
        member_name: member_name.to_string(),
    })
}

/// `JSON.stringify({ ...config, stateDir, members })`.
pub fn build_team_config_json(config: &TeamCoreConfig, state_dir: &StateDirConfig, members: &[String]) -> String {
    let mut object = match serde_json::to_value(config) {
        Ok(Value::Object(object)) => object,
        _ => Map::new(),
    };
    object.insert(
        "stateDir".to_string(),
        Value::String(resolve_state_dir(state_dir).to_string_lossy().into_owned()),
    );
    object.insert(
        "members".to_string(),
        Value::Array(members.iter().cloned().map(Value::String).collect()),
    );
    Value::Object(object).to_string()
}

fn dedupe(values: &[String]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    values
        .iter()
        .filter(|value| seen.insert(value.as_str()))
        .cloned()
        .collect()
}

pub fn create_team_member_respawn_launch_resolver(
    options: TeamMemberRespawnLaunchResolverOptions,
) -> Result<TeamMemberRespawnLaunchResolver, String> {
    let base_dir = team_storage_base_dir(&options.state_dir).to_string_lossy().into_owned();
    let config = to_team_core_config(&options.team_bounds, &base_dir)?;
    let inherited_extensions = dedupe(options.member_extension.inherited_extensions.as_deref().unwrap_or(&[]));
    let extensions = assemble_member_extensions(&options.member_extension.entry_path, &inherited_extensions);
    Ok(TeamMemberRespawnLaunchResolver {
        config,
        state_dir: options.state_dir,
        inherited_extensions,
        extensions,
    })
}

impl TeamMemberRespawnLaunchResolver {
    /// Resolves the trusted launch for a task record identified by its `name` and `task_id`.
    pub fn resolve(
        &self,
        name: Option<&str>,
        task_id: &str,
    ) -> Result<TeamMemberRespawnLaunch, TeamMemberRespawnLaunchError> {
        use TeamMemberRespawnLaunchErrorCode as Code;

        let Some(identity) = parse_team_member_task_name(name) else {
            return Ok(TeamMemberRespawnLaunch {
                extensions: self.inherited_extensions.clone(),
                member_env: None,
            });
        };
        let runtime = load_runtime_state(&identity.team_run_id, &self.config)
            .map_err(|_| TeamMemberRespawnLaunchError::new(Code::RuntimeUnavailable, &identity))?;
        let status = runtime.status.as_str();
        if status != "active" && status != "shutdown_requested" {
            return Err(TeamMemberRespawnLaunchError::new(Code::RuntimeInactive, &identity));
        }
        let member = runtime.members.iter().find(|entry| entry.name == identity.member_name);
        let shutdown_approved = member.is_some_and(|member| {
            serde_json::to_value(member.status)
                .ok()
                .is_some_and(|value| value.as_str() == Some("shutdown_approved"))
        });
        if member.is_none() || shutdown_approved {
            return Err(TeamMemberRespawnLaunchError::new(Code::MemberMissing, &identity));
        }
        let runtime_dir = resolve_team_runtime_dirs(&self.state_dir, &identity.team_run_id)
            .map_err(|_| TeamMemberRespawnLaunchError::new(Code::RuntimeUnavailable, &identity))?
            .runtime_dir;
        let map = read_member_task_map(&runtime_dir);
        if map.get(&identity.member_name).map(String::as_str) != Some(task_id) {
            return Err(TeamMemberRespawnLaunchError::new(Code::TaskMappingMismatch, &identity));
        }
        let members: Vec<String> = runtime.members.iter().map(|member| member.name.clone()).collect();
        let mut member_env = BTreeMap::new();
        member_env.insert(
            "SENPI_TASK_MEMBER".to_string(),
            format!("{}::{}", runtime.team_run_id, identity.member_name),
        );
        member_env.insert(MEMBER_TASK_ID_ENV.to_string(), task_id.to_string());
        member_env.insert(
            "SENPI_TASK_TEAM_CONFIG".to_string(),
            build_team_config_json(&self.config, &self.state_dir, &members),
        );
        Ok(TeamMemberRespawnLaunch {
            extensions: self.extensions.clone(),
            member_env: Some(member_env),
        })
    }
}
