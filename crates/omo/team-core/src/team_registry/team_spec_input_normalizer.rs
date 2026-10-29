//! `normalizeTeamSpecInput`: turn loosely written team specs into schema-shaped JSON.

use serde_json::{Map, Value};

use crate::error::{Result, TeamCoreError};
use crate::resolve_caller_team_lead::CallerTeamLead;

#[derive(Debug, Clone, Default)]
pub struct NormalizeTeamSpecInputOptions {
    pub caller_team_lead: Option<CallerTeamLead>,
    pub default_category_name: Option<String>,
}

type JsonRecord = Map<String, Value>;

fn omit_empty_string_fields(record: &JsonRecord) -> JsonRecord {
    record
        .iter()
        .filter(|(_, value)| value.as_str() != Some(""))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn get_member_name(value: &Value) -> Option<String> {
    value.as_object()?.get("name")?.as_str().map(str::to_owned)
}

pub(crate) fn normalize_name_stem(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let mut stem = String::with_capacity(lowered.len());
    let mut in_separator = false;
    for character in lowered.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            stem.push(character);
            in_separator = false;
        } else if !in_separator {
            stem.push('-');
            in_separator = true;
        }
    }
    let trimmed = stem.trim_matches('-');
    if trimmed.is_empty() {
        "member".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn derive_member_name_stem(member: &JsonRecord) -> String {
    let kind = member.get("kind").and_then(Value::as_str);
    match (
        kind,
        member.get("category").and_then(Value::as_str),
        member.get("subagent_type").and_then(Value::as_str),
    ) {
        (Some("category"), Some(category), _) => normalize_name_stem(category),
        (Some("subagent_type"), _, Some(subagent_type)) => normalize_name_stem(subagent_type),
        _ => "member".to_owned(),
    }
}

fn assign_generated_member_names(raw_members: Vec<Value>) -> Vec<Value> {
    let mut used_names: Vec<String> = Vec::new();
    raw_members
        .into_iter()
        .map(|member| {
            let Value::Object(mut record) = member else {
                return member;
            };
            let raw_name = record
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let stem = raw_name
                .as_deref()
                .map_or_else(|| derive_member_name_stem(&record), normalize_name_stem);
            let (mut generated, mut suffix) = match raw_name {
                None => (format!("{stem}-1"), 1),
                Some(_) => (stem.clone(), 2),
            };
            while used_names.contains(&generated) {
                generated = format!("{stem}-{suffix}");
                suffix += 1;
            }
            used_names.push(generated.clone());
            record.insert("name".to_owned(), Value::String(generated));
            Value::Object(record)
        })
        .collect()
}

fn has_member_lead_flag(raw_members: &[Value]) -> bool {
    raw_members
        .iter()
        .any(|member| member.get("isLead") == Some(&Value::Bool(true)))
}

fn create_caller_lead_member(caller_agent_type_id: &str) -> Value {
    serde_json::json!({
        "name": "lead",
        "kind": "subagent_type",
        "subagent_type": caller_agent_type_id,
    })
}

fn get_prompt_alias(member: &JsonRecord) -> Option<String> {
    ["prompt", "systemPrompt", "system_prompt"]
        .iter()
        .find_map(|key| member.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn format_string_array(value: Option<&Value>) -> Option<String> {
    let items = value?.as_array()?;
    let strings: Vec<&str> = items
        .iter()
        .filter_map(Value::as_str)
        .filter(|item| !item.trim().is_empty())
        .collect();
    (!strings.is_empty()).then(|| strings.join(", "))
}

fn build_prompt_from_natural_member(member: &JsonRecord) -> String {
    if let Some(alias) = get_prompt_alias(member) {
        return alias;
    }
    let parts: Vec<String> = [
        member
            .get("role")
            .and_then(Value::as_str)
            .map(|role| format!("Role: {role}")),
        member
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        format_string_array(member.get("capabilities")),
        format_string_array(member.get("responsibilities")),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.trim().is_empty())
    .collect();
    if parts.is_empty() {
        "Work on the assigned team task and report findings to the lead.".to_owned()
    } else {
        parts.join("\n")
    }
}

const NATURAL_MEMBER_KEYS: [&str; 9] = [
    "capabilities",
    "description",
    "loadSkills",
    "load_skills",
    "permission",
    "responsibilities",
    "role",
    "systemPrompt",
    "system_prompt",
];

fn set_category(member: &mut JsonRecord, category: &str) {
    member.insert("kind".to_owned(), Value::String("category".to_owned()));
    member.insert("category".to_owned(), Value::String(category.to_owned()));
}

fn set_kind(member: &mut JsonRecord, kind: &str) {
    member.insert("kind".to_owned(), Value::String(kind.to_owned()));
}

fn normalize_inline_member(
    member: &JsonRecord,
    options: &NormalizeTeamSpecInputOptions,
) -> JsonRecord {
    let stripped = omit_empty_string_fields(member);
    let mut normalized: JsonRecord = stripped
        .iter()
        .filter(|(key, _)| !NATURAL_MEMBER_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let raw_kind = normalized.get("kind").cloned();
    let has_category = normalized.get("category").is_some_and(Value::is_string);
    let has_subagent_type = normalized
        .get("subagent_type")
        .is_some_and(Value::is_string);
    let default_category = options.default_category_name.as_deref();

    match raw_kind.as_ref() {
        None => {
            if has_category {
                set_kind(&mut normalized, "category");
            } else if has_subagent_type {
                set_kind(&mut normalized, "subagent_type");
            } else if let Some(category) = default_category {
                set_category(&mut normalized, category);
            }
        }
        Some(Value::String(kind)) if kind == "category" || kind == "subagent_type" => {}
        Some(kind) => {
            if has_category {
                set_kind(&mut normalized, "category");
            } else if has_subagent_type {
                set_kind(&mut normalized, "subagent_type");
            } else if let Some(kind) = kind
                .as_str()
                .filter(|kind| !matches!(*kind, "agent" | "member" | "worker" | "analyst"))
            {
                set_category(&mut normalized, kind);
            } else if let Some(category) = default_category {
                set_category(&mut normalized, category);
            }
        }
    }

    if normalized.get("kind").and_then(Value::as_str) == Some("category")
        && !normalized.contains_key("prompt")
    {
        normalized.insert(
            "prompt".to_owned(),
            Value::String(build_prompt_from_natural_member(&stripped)),
        );
    }
    normalized
}

/// `normalizeTeamSpecInput`. Non-object input is returned unchanged.
pub fn normalize_team_spec_input(
    raw: &Value,
    options: Option<&NormalizeTeamSpecInputOptions>,
) -> Result<Value> {
    let default_options = NormalizeTeamSpecInputOptions::default();
    let options = options.unwrap_or(&default_options);
    let Some(raw_record) = raw.as_object() else {
        return Ok(raw.clone());
    };

    let mut spec = omit_empty_string_fields(raw_record);
    if let Some(name) = spec.get("name").and_then(Value::as_str) {
        let stem = normalize_name_stem(name);
        spec.insert("name".to_owned(), Value::String(stem));
    }

    let raw_lead = raw_record
        .get("lead")
        .and_then(Value::as_object)
        .filter(|lead| !omit_empty_string_fields(lead).is_empty());
    let mut lead_agent_id = spec
        .get("leadAgentId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let raw_members = raw_record.get("members").and_then(Value::as_array);
    let has_explicit_lead = lead_agent_id.is_some()
        || raw_lead.is_some()
        || raw_members.is_some_and(|members| has_member_lead_flag(members));

    if let Some(raw_members) = raw_members {
        let mut members: Vec<Value> = raw_members
            .iter()
            .map(|member| match member.as_object() {
                Some(record) => Value::Object(normalize_inline_member(record, options)),
                None => member.clone(),
            })
            .collect();
        let caller = options.caller_team_lead.as_ref();
        let use_first_member_as_lead = !has_explicit_lead
            && members.len() >= 8
            && caller.is_some_and(|caller| caller.is_eligible_for_team_lead);

        if let Some(raw_lead) = raw_lead {
            let mut lead_member = normalize_inline_member(raw_lead, options);
            if !lead_member.contains_key("name") {
                lead_member.insert("name".to_owned(), Value::String("lead".to_owned()));
            }
            let lead_name = lead_member
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let already_present = lead_name.as_ref().is_some_and(|lead_name| {
                members
                    .iter()
                    .any(|member| get_member_name(member).as_ref() == Some(lead_name))
            });
            if !already_present {
                members.insert(0, Value::Object(lead_member));
            }
            if lead_agent_id.is_none() {
                lead_agent_id = lead_name;
            }
        }

        if use_first_member_as_lead {
            lead_agent_id = members.first().and_then(get_member_name);
        } else if !has_explicit_lead {
            match caller {
                Some(CallerTeamLead {
                    is_eligible_for_team_lead: true,
                    agent_type_id: Some(agent_type_id),
                    ..
                }) => {
                    members.insert(0, create_caller_lead_member(agent_type_id));
                    lead_agent_id = Some("lead".to_owned());
                }
                Some(CallerTeamLead {
                    display_name: Some(display_name),
                    ..
                }) => {
                    return Err(TeamCoreError::message(format!(
                        "Caller agent {display_name} is not eligible as team lead; specify leadAgentId explicitly"
                    )));
                }
                _ => {}
            }
        }

        members = assign_generated_member_names(members);

        if lead_agent_id.is_none() && use_first_member_as_lead {
            lead_agent_id = members.first().and_then(get_member_name);
        }

        members = members
            .into_iter()
            .map(|member| {
                let Value::Object(mut record) = member else {
                    return member;
                };
                let is_lead = record.get("isLead") == Some(&Value::Bool(true));
                if lead_agent_id.is_none() && is_lead {
                    lead_agent_id = record
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                record.shift_remove("isLead");
                Value::Object(record)
            })
            .collect();

        if let Some(current) = lead_agent_id.clone()
            && !members
                .iter()
                .any(|member| get_member_name(member).as_deref() == Some(current.as_str()))
        {
            let normalized = normalize_name_stem(&current);
            if members
                .iter()
                .any(|member| get_member_name(member).as_deref() == Some(normalized.as_str()))
            {
                lead_agent_id = Some(normalized);
            }
        }

        if lead_agent_id.is_none() && members.len() == 1 {
            lead_agent_id = get_member_name(&members[0]);
        }

        spec.insert("members".to_owned(), Value::Array(members));
    }

    if let Some(lead_agent_id) = lead_agent_id {
        spec.insert("leadAgentId".to_owned(), Value::String(lead_agent_id));
    }
    spec.shift_remove("lead");
    Ok(Value::Object(spec))
}
