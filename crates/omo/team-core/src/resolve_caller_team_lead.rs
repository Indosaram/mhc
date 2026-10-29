//! Resolve the calling agent into a team-lead candidate.

use crate::types::{EligibilityVerdict, TeamSpec, agent_eligibility};

/// `CallerTeamLead`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallerTeamLead {
    pub agent_type_id: Option<String>,
    pub display_name: Option<String>,
    pub is_eligible_for_team_lead: bool,
}

const DISPLAY_NAME_TO_AGENT_TYPE: [(&str, &str); 6] = [
    ("sisyphus", "sisyphus"),
    ("sisyphus - ultraworker", "sisyphus"),
    ("hephaestus", "hephaestus"),
    ("hephaestus - implementation", "hephaestus"),
    ("atlas", "atlas"),
    ("sisyphus-junior", "sisyphus-junior"),
];

fn strip_agent_list_sort_prefix(raw_agent_name: &str) -> &str {
    raw_agent_name.trim_start_matches('\u{200B}').trim()
}

fn resolve_agent_type_id(display_name: &str) -> String {
    let lowered = display_name.to_lowercase();
    DISPLAY_NAME_TO_AGENT_TYPE
        .iter()
        .find(|(display, _)| *display == lowered)
        .map_or(lowered.clone(), |(_, agent)| (*agent).to_owned())
}

/// `resolveCallerTeamLead`.
#[must_use]
pub fn resolve_caller_team_lead(raw_agent_name: Option<&str>) -> CallerTeamLead {
    let Some(raw_agent_name) = raw_agent_name else {
        return CallerTeamLead::default();
    };
    let display_name = strip_agent_list_sort_prefix(raw_agent_name);
    if display_name.is_empty() {
        return CallerTeamLead::default();
    }
    let agent_type_id = resolve_agent_type_id(display_name);
    match agent_eligibility(&agent_type_id) {
        Some(entry) if entry.verdict != EligibilityVerdict::HardReject => CallerTeamLead {
            agent_type_id: Some(agent_type_id),
            display_name: Some(display_name.to_owned()),
            is_eligible_for_team_lead: true,
        },
        _ => CallerTeamLead {
            agent_type_id: None,
            display_name: Some(display_name.to_owned()),
            is_eligible_for_team_lead: false,
        },
    }
}

/// `shouldReuseCallerLeadSession` (a parsed spec always has a lead id).
#[must_use]
pub fn should_reuse_caller_lead_session(
    _spec: &TeamSpec,
    caller_agent_type_id: Option<&str>,
) -> bool {
    caller_agent_type_id.is_some()
}
