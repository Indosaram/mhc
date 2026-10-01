//! Senpi-local member vocabulary validation.

use team_core::types::TeamSpec;

use crate::agents::curated_readonly_agent_names;
use crate::team::errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode};

pub type MemberPredicate = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Senpi-local member vocabulary resolver ports. `is_category_resolvable` is backed by the category
/// resolution; `is_known_agent` is backed by the agent loader. Kept as injectable predicates so the
/// registry does not couple to a full model registry just to validate a spec.
pub struct SenpiTeamMemberPorts {
    pub is_category_resolvable: MemberPredicate,
    pub is_known_agent: MemberPredicate,
    pub category_names: Option<Vec<String>>,
    pub agent_names: Option<Vec<String>>,
}

const ALLOWED_KINDS_HINT: &str = "member kind must be 'category' (a resolvable delegate category), 'subagent_type' (a loaded agent definition), or the 'agent' alias for a subagent_type";

fn available_hint(label: &str, names: Option<&Vec<String>>) -> String {
    match names {
        Some(names) if !names.is_empty() => {
            let mut sorted = names.clone();
            sorted.sort();
            format!(" Available {label}: {}.", sorted.join(", "))
        }
        _ => String::new(),
    }
}

/// Validates parsed team members against the ACTUAL senpi vocabulary. team-core `validate_spec` is
/// NOT used because it hard-calls the opencode-roster eligibility registry.
pub fn validate_senpi_team_members(spec: &TeamSpec, ports: &SenpiTeamMemberPorts) -> Result<(), SenpiTeamSpecError> {
    let curated = curated_readonly_agent_names();
    for member in &spec.members {
        if member.kind.as_str() == "category" {
            let category = member.category.as_deref().unwrap_or_default();
            if !(ports.is_category_resolvable)(category) {
                let available = available_hint("categories", ports.category_names.as_ref());
                return Err(SenpiTeamSpecError::new(
                    format!(
                        "Team '{}' member '{}' references unknown category '{category}'.{available} {ALLOWED_KINDS_HINT}.",
                        spec.name, member.name
                    ),
                    SenpiTeamSpecErrorCode::UnresolvableCategory,
                    &spec.name,
                ));
            }
            continue;
        }

        let subagent_type = member.subagent_type.as_deref().unwrap_or_default();
        if curated.contains(subagent_type) {
            return Err(SenpiTeamSpecError::new(
                format!(
                    "curated read-only agent \"{subagent_type}\" cannot be a team member; delegate via the task tool instead"
                ),
                SenpiTeamSpecErrorCode::UnknownSubagentType,
                &spec.name,
            ));
        }

        if !(ports.is_known_agent)(subagent_type) {
            let available = available_hint("agents", ports.agent_names.as_ref());
            return Err(SenpiTeamSpecError::new(
                format!(
                    "Team '{}' member '{}' references unknown subagent_type '{subagent_type}'.{available} {ALLOWED_KINDS_HINT}.",
                    spec.name, member.name
                ),
                SenpiTeamSpecErrorCode::UnknownSubagentType,
                &spec.name,
            ));
        }
    }
    Ok(())
}
