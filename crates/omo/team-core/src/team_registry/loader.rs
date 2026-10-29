//! Load team specs from disk: read, parse, normalize, schema-check, validate.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::logger;
use crate::types::{SchemaIssues, SpecSource, TeamSpec};

use super::paths::{TeamSpecEntry, discover_team_specs, get_team_spec_path, resolve_base_dir};
use super::team_spec_input_normalizer::{NormalizeTeamSpecInputOptions, normalize_team_spec_input};
use super::validator::validate_spec;

/// One row of `loadAllTeamSpecs`: exactly one of `spec` / `error` is set.
#[derive(Debug)]
pub struct LoadedTeamSpec {
    pub name: String,
    pub scope: SpecSource,
    pub spec: Option<TeamSpec>,
    pub error: Option<TeamCoreError>,
}

fn create_special_case_validation_error(raw_spec: &Value) -> Option<TeamCoreError> {
    let raw_spec = raw_spec.as_object()?;
    let raw_members = raw_spec.get("members")?.as_array()?;
    if raw_members.len() > 8 {
        let team_name = raw_spec
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        return Some(TeamCoreError::spec_validation(
            format!("Team '{team_name}' exceeds max 8 members."),
            "TEAM_MEMBER_LIMIT_EXCEEDED",
            Some("members"),
            None,
        ));
    }
    for raw_member in raw_members {
        let Some(member) = raw_member.as_object() else {
            continue;
        };
        let member_name = member
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let has_kind = member.contains_key("kind");
        if member.contains_key("category") && member.contains_key("subagent_type") {
            return Some(TeamCoreError::spec_validation(
                format!(
                    "Member '{member_name}' specifies both 'category' and 'subagent_type'. Must specify exactly one via 'kind' discriminator."
                ),
                "AMBIGUOUS_MEMBER_KIND",
                Some("kind"),
                Some(member_name),
            ));
        }
        if !has_kind {
            return Some(TeamCoreError::spec_validation(
                format!(
                    "Member '{member_name}' missing 'kind' discriminator. Specify either {{kind:'category', category, prompt}} or {{kind:'subagent_type', subagent_type}}."
                ),
                "MISSING_MEMBER_KIND",
                Some("kind"),
                Some(member_name),
            ));
        }
        if member.get("kind").and_then(Value::as_str) == Some("category")
            && !member.contains_key("prompt")
        {
            let category = member
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            return Some(TeamCoreError::spec_validation(
                format!(
                    "Member '{member_name}' uses category '{category}' but is missing required 'prompt' field. Category members must supply a task prompt."
                ),
                "MISSING_CATEGORY_PROMPT",
                Some("prompt"),
                Some(member_name),
            ));
        }
    }
    None
}

fn create_schema_validation_error(raw_spec: &Value, issues: &SchemaIssues) -> TeamCoreError {
    if let Some(special) = create_special_case_validation_error(raw_spec) {
        return special;
    }
    let first = issues.first();
    let field = first
        .map(|issue| issue.path.join("."))
        .filter(|field| !field.is_empty());
    match (field, first) {
        (Some(field), Some(issue)) => TeamCoreError::spec_validation(
            format!("Invalid team spec field '{field}': {}", issue.message),
            "INVALID_TEAM_SPEC",
            Some(&field),
            None,
        ),
        _ => TeamCoreError::spec_validation(
            format!("Invalid team spec: {issues}"),
            "INVALID_TEAM_SPEC",
            None,
            None,
        ),
    }
}

fn load_team_spec_from_entry(
    entry: &TeamSpecEntry,
    options: Option<&NormalizeTeamSpecInputOptions>,
) -> Result<TeamSpec> {
    let raw_text = fs::read_to_string(&entry.path).map_err(|error| {
        TeamCoreError::spec_validation(
            format!("Failed to read team spec '{}': {error}", entry.name),
            "TEAM_SPEC_READ_FAILED",
            None,
            None,
        )
    })?;
    let raw_spec: Value = serde_json::from_str(&raw_text).map_err(|error| {
        TeamCoreError::spec_validation(
            format!("Failed to parse team spec '{}' JSON: {error}", entry.name),
            "INVALID_JSON",
            None,
            None,
        )
    })?;
    let normalized = normalize_team_spec_input(&raw_spec, options)?;
    let spec = TeamSpec::safe_parse(&normalized)
        .map_err(|issues| create_schema_validation_error(&normalized, &issues))?;
    validate_spec(&spec)?;
    Ok(spec)
}

/// `loadTeamSpec`: the project-scoped spec wins over the user-scoped one.
pub fn load_team_spec(
    team_name: &str,
    config: &TeamModeConfig,
    project_root: &Path,
    options: Option<&NormalizeTeamSpecInputOptions>,
) -> Result<TeamSpec> {
    let discovered = discover_team_specs(config, project_root);
    let Some(entry) = discovered.iter().find(|entry| entry.name == team_name) else {
        let base_dir = resolve_base_dir(config);
        let project_path = get_team_spec_path(
            &base_dir,
            team_name,
            SpecSource::Project,
            Some(project_root),
        );
        let user_path = get_team_spec_path(&base_dir, team_name, SpecSource::User, None);
        return Err(TeamCoreError::spec_validation(
            format!(
                "Team '{team_name}' was not found. Expected '{}' or '{}'.",
                project_path.display(),
                user_path.display()
            ),
            "TEAM_SPEC_NOT_FOUND",
            Some("name"),
            None,
        ));
    };
    load_team_spec_from_entry(entry, options)
}

/// `loadAllTeamSpecs`: failures are returned as data and logged.
#[must_use]
pub fn load_all_team_specs(config: &TeamModeConfig, project_root: &Path) -> Vec<LoadedTeamSpec> {
    discover_team_specs(config, project_root)
        .into_iter()
        .map(|entry| match load_team_spec_from_entry(&entry, None) {
            Ok(spec) => LoadedTeamSpec {
                name: entry.name,
                scope: entry.scope,
                spec: Some(spec),
                error: None,
            },
            Err(error) => {
                logger::log(
                    "team-spec load failed",
                    Some(json!({
                        "event": "team-spec-load-failed",
                        "teamName": entry.name,
                        "scope": entry.scope.as_str(),
                        "path": entry.path,
                        "error": error.to_string(),
                    })),
                );
                LoadedTeamSpec {
                    name: entry.name,
                    scope: entry.scope,
                    spec: None,
                    error: Some(error),
                }
            }
        })
        .collect()
}
