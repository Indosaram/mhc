//! Semantic team-spec validation beyond the schema.

use crate::error::{Result, TeamCoreError};
use crate::types::{EligibilityVerdict, Member, MemberKind, TeamSpec, agent_eligibility};

const MAX_TEAM_MEMBERS: usize = 8;
const HYPERPLAN_REQUIRED_CATEGORIES: [&str; 4] = [
    "unspecified-low",
    "unspecified-high",
    "ultrabrain",
    "artistry",
];
const UNKNOWN_SUBAGENT_MESSAGE: &str = "Unknown subagent_type '<name>'. Available ELIGIBLE agents: sisyphus, atlas, sisyphus-junior, hephaestus (if D-36 applied). Use delegate-task for read-only agents like oracle, librarian, explore, metis, momus, multimodal-looker.";

/// `validateSpec`.
pub fn validate_spec(spec: &TeamSpec) -> Result<()> {
    if spec.members.len() > MAX_TEAM_MEMBERS {
        return Err(TeamCoreError::spec_validation(
            format!("Team '{}' exceeds max 8 members.", spec.name),
            "TEAM_MEMBER_LIMIT_EXCEEDED",
            Some("members"),
            None,
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut lead_match_count = 0;
    for member in &spec.members {
        if seen.contains(&member.name.as_str()) {
            return Err(TeamCoreError::spec_validation(
                format!(
                    "Member name '{}' is duplicated within team '{}'. Member names must be unique.",
                    member.name, spec.name
                ),
                "DUPLICATE_MEMBER_NAME",
                Some("members"),
                Some(&member.name),
            ));
        }
        seen.push(&member.name);
        validate_member_eligibility(member)?;
        validate_dual_support(member)?;
        if member.name == spec.lead_agent_id {
            lead_match_count += 1;
        }
    }
    if lead_match_count != 1 {
        return Err(TeamCoreError::spec_validation(
            format!(
                "Team '{}' leadAgentId '{}' must match exactly one member.name.",
                spec.name, spec.lead_agent_id
            ),
            "INVALID_LEAD_AGENT_ID",
            Some("leadAgentId"),
            None,
        ));
    }
    validate_hyperplan_composition(spec)
}

fn validate_hyperplan_composition(spec: &TeamSpec) -> Result<()> {
    if spec.name != "hyperplan" {
        return Ok(());
    }
    let categories: Vec<&str> = spec
        .members
        .iter()
        .filter(|member| member.kind == MemberKind::Category)
        .filter_map(|member| member.category.as_deref())
        .collect();
    for category in HYPERPLAN_REQUIRED_CATEGORIES {
        if !categories.contains(&category) {
            return Err(TeamCoreError::spec_validation(
                format!("Hyperplan team must include category '{category}'."),
                "HYPERPLAN_REQUIRED_CATEGORY_MISSING",
                Some("members"),
                None,
            ));
        }
    }
    Ok(())
}

/// `validateMemberEligibility`.
pub fn validate_member_eligibility(member: &Member) -> Result<()> {
    if member.kind != MemberKind::SubagentType {
        return Ok(());
    }
    let subagent_type = member.subagent_type.as_deref().unwrap_or_default();
    let Some(eligibility) = agent_eligibility(subagent_type) else {
        return Err(TeamCoreError::spec_validation(
            UNKNOWN_SUBAGENT_MESSAGE.replacen("<name>", subagent_type, 1),
            "UNKNOWN_SUBAGENT_TYPE",
            Some("subagent_type"),
            Some(&member.name),
        ));
    };
    if eligibility.verdict == EligibilityVerdict::HardReject {
        let message = eligibility.rejection_message.map_or_else(
            || format!("Agent '{subagent_type}' is not eligible as a team member."),
            str::to_owned,
        );
        return Err(TeamCoreError::spec_validation(
            message,
            "INELIGIBLE_AGENT",
            Some("subagent_type"),
            Some(&member.name),
        ));
    }
    Ok(())
}

/// `validateDualSupport`.
pub fn validate_dual_support(member: &Member) -> Result<()> {
    let trimmed = member.prompt.as_deref().map(str::trim);
    if trimmed == Some("") {
        return Err(TeamCoreError::spec_validation(
            format!(
                "Member '{}' prompt must not be empty after trimming whitespace.",
                member.name
            ),
            "EMPTY_PROMPT",
            Some("prompt"),
            Some(&member.name),
        ));
    }
    if member.kind == MemberKind::Category && trimmed.unwrap_or_default().encode_utf16().count() < 8
    {
        return Err(TeamCoreError::spec_validation(
            format!(
                "Member '{}' category prompt must be at least 8 characters long.",
                member.name
            ),
            "CATEGORY_PROMPT_TOO_SHORT",
            Some("prompt"),
            Some(&member.name),
        ));
    }
    Ok(())
}
