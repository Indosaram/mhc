//! `tools/task/plan-review-contract.ts`: the plan-review prompt contract for one-shot review
//! agents (momus). The child receives EXACTLY one canonical sentence carrying one
//! `.omo/plans/*.md` path. Target precedence: an explicit single path in the caller prompt that
//! matches a session-recorded reference; otherwise the session's most-referenced plan artifact
//! (count desc, recency tie-break); otherwise the spawn is denied.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::agents::{PlanArtifactReference, SkillInvocationState, interaction_policy_for_agent};
use crate::tools::task::invocation_gate::{SkillInvocationResolver, with_session_skill_state};
pub use crate::tools::task::spawn_policy::PlanReviewContractOutcome;

static PLAN_PATH_GLOBAL: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)[^\s"'`()\[\]]*\.omo[\\/]plans[\\/][^\s"'`()\[\]/\\]+\.md"#).ok()
});

pub const PLAN_REVIEW_DENY_MESSAGE: &str = "momus requires exactly one .omo/plans/*.md path: include the plan path in the prompt, or touch the plan file in this session first.";

pub fn extract_plan_paths(text: &str) -> Vec<String> {
    let Some(pattern) = PLAN_PATH_GLOBAL.as_ref() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for found in pattern.find_iter(text) {
        let normalized = found.as_str().replace('\\', "/");
        if seen.insert(normalized.clone()) {
            paths.push(normalized);
        }
    }
    paths
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReviewTarget {
    Path { path: String },
    Deny { message: String },
}

pub fn resolve_plan_review_target(
    caller_prompt: &str,
    state: &dyn SkillInvocationState,
) -> PlanReviewTarget {
    let references = state.plan_artifact_references();
    let explicit = extract_plan_paths(caller_prompt);
    if let [only] = explicit.as_slice()
        && let Some(recorded) = match_recorded_reference(only, &references)
    {
        return PlanReviewTarget::Path {
            path: recorded.path.clone(),
        };
    }
    if let Some(most_referenced) = top_reference(&references) {
        return PlanReviewTarget::Path {
            path: most_referenced.path.clone(),
        };
    }
    PlanReviewTarget::Deny {
        message: PLAN_REVIEW_DENY_MESSAGE.to_string(),
    }
}

/// An explicit caller path is a SELECTOR over session-recorded references, never a value that
/// reaches the child: the prompt is built from the RECORDED path. Identity is the normalized
/// `.omo/plans/<name>.md` suffix, which keeps worktree-rooted recordings matchable.
fn match_recorded_reference<'a>(
    candidate: &str,
    references: &'a [PlanArtifactReference],
) -> Option<&'a PlanArtifactReference> {
    let wanted = plan_suffix(candidate)?;
    references
        .iter()
        .find(|reference| plan_suffix(&reference.path).as_deref() == Some(wanted.as_str()))
}

fn plan_suffix(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let index = normalized.find(".omo/plans/")?;
    Some(normalized[index..].to_string())
}

pub fn build_plan_review_prompt(path: &str) -> String {
    format!("Review the work plan at {path} for contradictions and blocking issues.")
}

/// Resolves the plan-review contract outcome directly from a `SkillInvocationState`, without
/// going through a session-keyed resolver. `None` when `subagent_type` carries no plan-review
/// contract (passthrough), otherwise the forced prompt or the denial.
pub fn resolve_plan_review_contract(
    subagent_type: &str,
    caller_prompt: &str,
    state: &dyn SkillInvocationState,
) -> Option<PlanReviewContractOutcome> {
    interaction_policy_for_agent(subagent_type)?;
    Some(match resolve_plan_review_target(caller_prompt, state) {
        PlanReviewTarget::Deny { message } => PlanReviewContractOutcome::Deny { message },
        PlanReviewTarget::Path { path } => PlanReviewContractOutcome::Prompt {
            prompt: build_plan_review_prompt(&path),
        },
    })
}

/// Returns `None` when the target agent carries no plan-review contract (passthrough), otherwise
/// the forced prompt or the denial. A missing resolver fails CLOSED into the empty state, which
/// yields the deny branch for contract agents. The only agent carrying an interaction policy
/// (momus) is the plan-review contract agent.
pub fn plan_review_contract_outcome(
    resolver: Option<&SkillInvocationResolver>,
    subagent_type: &str,
    caller_prompt: &str,
    session_id: &str,
) -> Option<PlanReviewContractOutcome> {
    interaction_policy_for_agent(subagent_type)?;
    let target = with_session_skill_state(resolver, session_id, |state| {
        resolve_plan_review_target(caller_prompt, state)
    });
    Some(match target {
        PlanReviewTarget::Deny { message } => PlanReviewContractOutcome::Deny { message },
        PlanReviewTarget::Path { path } => PlanReviewContractOutcome::Prompt {
            prompt: build_plan_review_prompt(&path),
        },
    })
}

fn top_reference(references: &[PlanArtifactReference]) -> Option<&PlanArtifactReference> {
    let mut top: Option<&PlanArtifactReference> = None;
    for reference in references {
        let replace = top.is_none_or(|current| {
            reference.count > current.count
                || (reference.count == current.count
                    && reference.last_touched_at > current.last_touched_at)
        });
        if replace {
            top = Some(reference);
        }
    }
    top
}
