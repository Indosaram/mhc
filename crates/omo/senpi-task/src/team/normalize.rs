//! Senpi team spec normalization on top of team-core's lenient input normalizer.

use serde_json::{Map, Value};
use team_core::team_registry::{NormalizeTeamSpecInputOptions, normalize_team_spec_input};
use team_core::types::TeamSpec;

use crate::task_summary::clamp_task_summary;
use crate::team::errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode};

/// Sentinel identity for the current senpi session acting as the team lead. It is written as
/// `leadAgentId` and is NEVER a spawnable member. team-core's `validateSpec` would try to match it
/// to a member and reject; we deliberately parse with `TeamSpec::safe_parse` and validate members
/// locally instead.
pub const TEAM_LEAD_SENTINEL: &str = "lead";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizeSenpiTeamSpecOptions {
    pub caller_team_lead: Option<Value>,
}

fn assert_no_caller_team_lead(
    options: Option<&NormalizeSenpiTeamSpecOptions>,
    team_name: &str,
) -> Result<(), SenpiTeamSpecError> {
    if options.is_some_and(|options| options.caller_team_lead.is_some()) {
        return Err(SenpiTeamSpecError::new(
            format!(
                "Team '{team_name}' passed a callerTeamLead option, which is not supported: the current senpi session is always the '{TEAM_LEAD_SENTINEL}' sentinel and team-core would otherwise insert a spawnable lead member."
            ),
            SenpiTeamSpecErrorCode::ReservedCallerTeamLead,
            team_name,
        ));
    }
    Ok(())
}

fn coerce_string_spec(raw_spec: &Value, team_name: &str) -> Result<Value, SenpiTeamSpecError> {
    let Value::String(text) = raw_spec else {
        return Ok(raw_spec.clone());
    };
    serde_json::from_str::<Value>(text).map_err(|error| {
        SenpiTeamSpecError::new(
            format!(
                "Team '{team_name}' spec is a string that is not valid JSON ({error}). Pass the spec as an object like {{ name?, members: [{{ name, category|subagent_type, prompt? }}] }}, or as a valid JSON string of that object."
            ),
            SenpiTeamSpecErrorCode::InvalidSpec,
            team_name,
        )
    })
}

fn wrap_single_member(raw_spec: Value) -> Value {
    match raw_spec {
        Value::Object(mut record) => {
            if let Some(members) = record.get_mut("members").filter(|members| members.is_object()) {
                let single = members.take();
                *members = Value::Array(vec![single]);
            }
            Value::Object(record)
        }
        other => other,
    }
}

fn describe_received(raw_spec: &Value) -> &'static str {
    match raw_spec {
        Value::Null => "null",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
        Value::String(_) => "a string",
        Value::Number(_) => "a number",
        Value::Bool(_) => "a boolean",
    }
}

fn not_an_object_error(raw_spec: &Value, team_name: &str) -> SenpiTeamSpecError {
    SenpiTeamSpecError::new(
        format!(
            "Team '{team_name}' spec must be an object like {{ name?, members: [...] }}; received {}. Pass the spec as a nested object, or as a valid JSON string of that object.",
            describe_received(raw_spec)
        ),
        SenpiTeamSpecErrorCode::InvalidSpec,
        team_name,
    )
}

fn assert_no_raw_lead_field(raw_spec: &Value, team_name: &str) -> Result<(), SenpiTeamSpecError> {
    let has_lead = raw_spec
        .as_object()
        .and_then(|record| record.get("lead"))
        .is_some_and(|lead| !lead.is_null());
    if has_lead {
        return Err(SenpiTeamSpecError::new(
            format!(
                "Team '{team_name}' declares a 'lead' field. '{TEAM_LEAD_SENTINEL}' is reserved for the current-session sentinel; declare workers under 'members' only."
            ),
            SenpiTeamSpecErrorCode::ReservedLeadField,
            team_name,
        ));
    }
    Ok(())
}

fn remap_agent_alias_kind(members: Vec<Value>) -> Vec<Value> {
    members
        .into_iter()
        .map(|mut member| {
            if let Some(record) = member
                .as_object_mut()
                .filter(|record| record.get("kind").and_then(Value::as_str) == Some("agent"))
            {
                record.insert("kind".to_string(), Value::String("subagent_type".to_string()));
            }
            member
        })
        .collect()
}

// Harness-side length enforcement mirroring the task tool: an over-limit member task_summary is
// clamped BEFORE TeamSpec::safe_parse so it truncates instead of rejecting the whole spec.
fn clamp_member_task_summaries(members: Vec<Value>) -> Vec<Value> {
    members
        .into_iter()
        .map(|mut member| {
            if let Some(record) = member.as_object_mut() {
                let summary = record.get("task_summary").and_then(Value::as_str).map(str::to_string);
                if let Some(summary) = summary {
                    match clamp_task_summary(Some(&summary)) {
                        Some(clamped) => {
                            record.insert("task_summary".to_string(), Value::String(clamped));
                        }
                        None => {
                            record.remove("task_summary");
                        }
                    }
                }
            }
            member
        })
        .collect()
}

fn assert_no_reserved_member_name(members: &[Value], team_name: &str) -> Result<(), SenpiTeamSpecError> {
    let reserved = members.iter().any(|member| {
        member
            .as_object()
            .and_then(|record| record.get("name"))
            .and_then(Value::as_str)
            == Some(TEAM_LEAD_SENTINEL)
    });
    if reserved {
        return Err(SenpiTeamSpecError::new(
            format!(
                "Team '{team_name}' has a member named '{TEAM_LEAD_SENTINEL}', which is reserved for the current-session sentinel. Rename the member."
            ),
            SenpiTeamSpecErrorCode::ReservedLeadMember,
            team_name,
        ));
    }
    Ok(())
}

/// Normalizes a raw senpi team spec (from an `omo.json` `teams` value or an `.omo/teams/<name>`
/// `config.json`) into a parsed team-core `TeamSpec`.
///
/// Pipeline: reject the three reserved-name paths, run team-core `normalize_team_spec_input` WITHOUT
/// the `callerTeamLead` option, apply the senpi pre-normalizer (map the `agent` alias to
/// `subagent_type`, inject the record key as `name`, always set the `lead` sentinel), then parse with
/// `TeamSpec::safe_parse`. team-core `validate_spec` / `load_team_spec` are never called: both
/// hard-call the opencode-roster eligibility check.
pub fn normalize_senpi_team_spec(
    raw_spec: &Value,
    team_name: &str,
    options: Option<&NormalizeSenpiTeamSpecOptions>,
) -> Result<TeamSpec, SenpiTeamSpecError> {
    assert_no_caller_team_lead(options, team_name)?;
    let coerced = wrap_single_member(coerce_string_spec(raw_spec, team_name)?);
    assert_no_raw_lead_field(&coerced, team_name)?;

    let normalized = match normalize_team_spec_input(&coerced, Some(&NormalizeTeamSpecInputOptions::default())) {
        Ok(Value::Object(normalized)) => normalized,
        Ok(_) => return Err(not_an_object_error(raw_spec, team_name)),
        Err(_) if !coerced.is_object() => return Err(not_an_object_error(raw_spec, team_name)),
        Err(error) => {
            return Err(SenpiTeamSpecError::new(
                format!("{error}"),
                SenpiTeamSpecErrorCode::InvalidSpec,
                team_name,
            ));
        }
    };

    let mut pre_normalized: Map<String, Value> = normalized;
    if let Some(Value::Array(members)) = pre_normalized.get_mut("members") {
        let remapped = clamp_member_task_summaries(remap_agent_alias_kind(std::mem::take(members)));
        assert_no_reserved_member_name(&remapped, team_name)?;
        *members = remapped;
    }
    if pre_normalized.get("name").is_none_or(Value::is_null) {
        pre_normalized.insert("name".to_string(), Value::String(team_name.to_string()));
    }
    pre_normalized.insert("leadAgentId".to_string(), Value::String(TEAM_LEAD_SENTINEL.to_string()));

    TeamSpec::safe_parse(&Value::Object(pre_normalized)).map_err(|issues| {
        let detail = match issues.first() {
            Some(issue) => {
                let path = issue.path.join(".");
                let path = if path.is_empty() { "spec".to_string() } else { path };
                format!("{path}: {}", issue.message)
            }
            None => "Invalid input".to_string(),
        };
        SenpiTeamSpecError::new(
            format!("Invalid team '{team_name}' spec ({detail})."),
            SenpiTeamSpecErrorCode::InvalidSpec,
            team_name,
        )
    })
}
