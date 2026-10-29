//! Senpi team registry: discovers named team specs from the project `.omo/teams` directory and the
//! raw `omo.json` `teams` section.

use std::collections::HashSet;
use std::path::Path;

use serde_json::{Map, Value};
use team_core::types::TeamSpec;

use crate::team::errors::SenpiTeamSpecError;
use crate::team::member_validator::{SenpiTeamMemberPorts, validate_senpi_team_members};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::storage::resolve_project_team_spec_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeamSpecSource {
    Project,
    OmoJson,
}

impl TeamSpecSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::OmoJson => "omo-json",
        }
    }
}

impl std::fmt::Display for TeamSpecSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct TeamRegistryEntry {
    pub name: String,
    pub source: TeamSpecSource,
    pub spec: TeamSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRegistryError {
    pub name: String,
    pub source: TeamSpecSource,
    pub code: String,
    pub message: String,
}

pub struct LoadTeamRegistryInput<'a> {
    pub project_root: &'a Path,
    pub omo_teams: Option<&'a Map<String, Value>>,
    pub ports: &'a SenpiTeamMemberPorts,
}

#[derive(Debug, Clone, Default)]
pub struct LoadTeamRegistryResult {
    pub teams: Vec<TeamRegistryEntry>,
    pub errors: Vec<TeamRegistryError>,
}

struct RawProjectSpec {
    name: String,
    raw_text: String,
}

fn to_registry_error(name: &str, source: TeamSpecSource, error: &SenpiTeamSpecError) -> TeamRegistryError {
    TeamRegistryError {
        name: name.to_string(),
        source,
        code: error.code.as_str().to_string(),
        message: error.message.clone(),
    }
}

fn read_project_team_specs(project_root: &Path) -> Vec<RawProjectSpec> {
    let teams_dir = project_root.join(".omo").join("teams");
    let Ok(entries) = std::fs::read_dir(&teams_dir) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();

    let mut candidates = Vec::new();
    for name in names {
        let Ok(raw_text) = std::fs::read_to_string(resolve_project_team_spec_path(project_root, &name)) else {
            continue;
        };
        candidates.push(RawProjectSpec { name, raw_text });
    }
    candidates
}

fn ingest(
    name: &str,
    source: TeamSpecSource,
    raw_spec: &Value,
    ports: &SenpiTeamMemberPorts,
    result: &mut LoadTeamRegistryResult,
) {
    let outcome = normalize_senpi_team_spec(raw_spec, name, None)
        .and_then(|spec| validate_senpi_team_members(&spec, ports).map(|()| spec));
    match outcome {
        Ok(spec) => result.teams.push(TeamRegistryEntry {
            name: name.to_string(),
            source,
            spec,
        }),
        Err(error) => result.errors.push(to_registry_error(name, source, &error)),
    }
}

/// Loads named team specs from the two project-scoped sources: the raw `omo.json` `teams` section and
/// the omo-compatible `<projectRoot>/.omo/teams/<name>/config.json` directory specs. The project
/// directory beats `omo.json` on a name collision. Each candidate is normalized + parsed via
/// `normalize_senpi_team_spec` (team-core normalizer + schema, never `validate_spec`) and checked by
/// the senpi-local member validator; a failing team is recorded in `errors` and spawns zero members.
pub fn load_team_registry(input: &LoadTeamRegistryInput<'_>) -> LoadTeamRegistryResult {
    let mut result = LoadTeamRegistryResult::default();

    let project_specs = read_project_team_specs(input.project_root);
    let project_names: HashSet<&str> = project_specs.iter().map(|candidate| candidate.name.as_str()).collect();

    for candidate in &project_specs {
        let raw_spec = match serde_json::from_str::<Value>(&candidate.raw_text) {
            Ok(raw_spec) => raw_spec,
            Err(error) => {
                result.errors.push(TeamRegistryError {
                    name: candidate.name.clone(),
                    source: TeamSpecSource::Project,
                    code: "INVALID_JSON".to_string(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        ingest(&candidate.name, TeamSpecSource::Project, &raw_spec, input.ports, &mut result);
    }

    if let Some(omo_teams) = input.omo_teams {
        for (name, raw_spec) in omo_teams {
            if project_names.contains(name.as_str()) {
                continue;
            }
            ingest(name, TeamSpecSource::OmoJson, raw_spec, input.ports, &mut result);
        }
    }

    result
}
