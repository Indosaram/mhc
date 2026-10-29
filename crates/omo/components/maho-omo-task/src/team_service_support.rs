//! Team vocabulary, spec resolution and shutdown transport composition.
use std::{collections::BTreeSet, path::Path};
use serde_json::{Map, Value};
use senpi_task::{category::BUILTIN_CATEGORY_DEFAULTS, manager::TaskManager, state::DeliverAs, steering::{CancelOptions, SendInput}, store::StateDirConfig, team::{errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode}, member_map::read_member_task_map, member_validator::{SenpiTeamMemberPorts, validate_senpi_team_members}, normalize::normalize_senpi_team_spec, registry::{LoadTeamRegistryInput, TeamSpecSource, load_team_registry}, shutdown::ShutdownOutboundMessage, storage::resolve_team_runtime_dirs}};
use team_core::types::TeamSpec;

pub struct ResolvedTeamSpec { pub spec: TeamSpec, pub source: TeamSpecSource }

pub fn build_member_ports(config: &Value, agent_names: &BTreeSet<String>) -> SenpiTeamMemberPorts {
    let mut categories: Vec<String> = BUILTIN_CATEGORY_DEFAULTS.iter().map(|entry| entry.name.to_owned()).collect();
    if let Some(custom) = config.get("categories").and_then(Value::as_object) {
        for name in custom.keys() { if !categories.contains(name) { categories.push(name.clone()); } }
    }
    let category_set: BTreeSet<String> = categories.iter().cloned().collect();
    let agents = agent_names.clone();
    SenpiTeamMemberPorts { is_category_resolvable: Box::new(move |name| category_set.contains(name)), is_known_agent: Box::new(move |name| agents.contains(name)), category_names: Some(categories), agent_names: Some(agent_names.iter().cloned().collect()) }
}

pub fn resolve_team_spec(team_name: Option<&str>, inline_spec: Option<&Value>, ports: &SenpiTeamMemberPorts, project_root: &Path, omo_teams: Option<&Map<String, Value>>) -> Result<ResolvedTeamSpec, SenpiTeamSpecError> {
    if let Some(raw) = inline_spec {
        let name = raw.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or("inline-team");
        let spec = normalize_senpi_team_spec(raw, name, None)?;
        validate_senpi_team_members(&spec, ports)?;
        return Ok(ResolvedTeamSpec { spec, source: TeamSpecSource::OmoJson });
    }
    let name = team_name.ok_or_else(|| SenpiTeamSpecError::new("no team_name or inline_spec provided", SenpiTeamSpecErrorCode::InvalidSpec, "unknown"))?;
    let registry = load_team_registry(&LoadTeamRegistryInput { project_root, omo_teams, ports });
    if let Some(entry) = registry.teams.iter().find(|entry| entry.name == name) { return Ok(ResolvedTeamSpec { spec: entry.spec.clone(), source: entry.source }); }
    if let Some(error) = registry.errors.iter().find(|error| error.name == name) { return Err(SenpiTeamSpecError::new(&error.message, SenpiTeamSpecErrorCode::InvalidSpec, name)); }
    let mut declared: Vec<&str> = registry.teams.iter().map(|entry| entry.name.as_str()).collect(); declared.sort();
    let mut broken: Vec<&str> = registry.errors.iter().map(|entry| entry.name.as_str()).collect(); broken.sort();
    let declared_suffix = if declared.is_empty() { String::new() } else { format!(" Declared teams: {}.", declared.join(", ")) };
    let broken_suffix = if broken.is_empty() { String::new() } else { format!(" ({} team spec(s) failed to load: {}.)", broken.len(), broken.join(", ")) };
    Err(SenpiTeamSpecError::new(format!("team '{name}' not found in project .omo/teams or omo.json.{declared_suffix}{broken_suffix}"), SenpiTeamSpecErrorCode::InvalidSpec, name))
}

fn member_task_id(state_dir: &StateDirConfig, run: &str, member: &str) -> Result<Option<String>, String> {
    let dirs = resolve_team_runtime_dirs(state_dir, run).map_err(|error| error.to_string())?;
    Ok(read_member_task_map(&dirs.runtime_dir).get(member).cloned())
}
pub fn make_shutdown_messenger<'a>(manager: &'a TaskManager, state_dir: &'a StateDirConfig, run: &'a str) -> impl Fn(&ShutdownOutboundMessage) -> Result<(), String> + 'a {
    move |message| {
        if let Some(task_id) = member_task_id(state_dir, run, &message.to)? {
            manager.send_to_task(&SendInput { id_or_name: task_id, message: format!("[team {}] {}", message.kind.as_str(), message.body), deliver_as: Some(DeliverAs::Steer), ..SendInput::default() }).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
pub fn make_cancel_member_task<'a>(manager: &'a TaskManager, state_dir: &'a StateDirConfig, run: &'a str) -> impl Fn(&str) -> Result<(), String> + 'a {
    move |member| {
        if let Some(task_id) = member_task_id(state_dir, run, member)? { manager.cancel_task(&task_id, Some(&format!("team {run} shutdown approved")), CancelOptions::default()).map_err(|error| error.to_string())?; }
        Ok(())
    }
}
