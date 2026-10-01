//! `tools/task/execute-plan-review-contract.test.ts`

use pretty_assertions::assert_eq;

use crate::agents::{PlanArtifactReference, SkillInvocationState, interaction_policy_for_agent};
use crate::tools::task::plan_review_contract::{
    PLAN_REVIEW_DENY_MESSAGE, PlanReviewContractOutcome, PlanReviewTarget,
    build_plan_review_prompt, extract_plan_paths, plan_review_contract_outcome,
    resolve_plan_review_target,
};

const PLAN_A: &str = ".omo/plans/alpha-plan.md";
const PLAN_B: &str = ".omo/plans/beta-plan.md";

fn canonical(path: &str) -> String {
    format!("Review the work plan at {path} for contradictions and blocking issues.")
}

/// Session fake mirroring the TS `resolverFor` state; references are rebuilt per call.
struct FakeState {
    requested: Vec<String>,
    artifact: bool,
    references: fn() -> Vec<PlanArtifactReference>,
}

impl SkillInvocationState for FakeState {
    fn has_invoked(&self, _skill_name: &str) -> bool {
        false
    }

    fn has_user_requested(&self, skill_name: &str) -> bool {
        self.requested.iter().any(|name| name == skill_name)
    }

    fn has_plan_artifact(&self) -> bool {
        self.artifact
    }

    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> {
        (self.references)()
    }
}

fn open_session_refs() -> Vec<PlanArtifactReference> {
    vec![
        PlanArtifactReference {
            path: PLAN_A.to_string(),
            count: 3,
            last_touched_at: 2,
        },
        PlanArtifactReference {
            path: PLAN_B.to_string(),
            count: 1,
            last_touched_at: 1,
        },
    ]
}

fn no_refs() -> Vec<PlanArtifactReference> {
    Vec::new()
}

fn open_session() -> FakeState {
    FakeState {
        requested: vec!["ulw-plan".to_string()],
        artifact: true,
        references: open_session_refs,
    }
}

fn empty_refs_session() -> FakeState {
    FakeState {
        requested: vec!["ulw-plan".to_string()],
        artifact: true,
        references: no_refs,
    }
}

/// The contract path for the momus agent: the forced child prompt, or the denial message.
fn momus_contract(prompt: &str, state: &FakeState) -> Result<String, String> {
    assert!(interaction_policy_for_agent("momus").is_some());
    match resolve_plan_review_target(prompt, state) {
        PlanReviewTarget::Path { path } => Ok(build_plan_review_prompt(&path)),
        PlanReviewTarget::Deny { message } => Err(message),
    }
}

#[test]
fn chatty_no_path_prompt_uses_most_referenced_plan() {
    // given
    let state = open_session();

    // when
    let result = momus_contract(
        "please review my plan very carefully, it is really important and well thought out",
        &state,
    );

    // then
    assert_eq!(result, Ok(canonical(PLAN_A)));
}

#[test]
fn explicit_single_plan_path_beats_most_referenced_plan() {
    let state = open_session();

    let result = momus_contract(&format!("review {PLAN_B} please"), &state);

    assert_eq!(result, Ok(canonical(PLAN_B)));
}

#[test]
fn two_plan_paths_fall_back_to_most_referenced_plan() {
    let state = open_session();

    let result = momus_contract(&format!("compare {PLAN_A} with {PLAN_B}"), &state);

    assert_eq!(result, Ok(canonical(PLAN_A)));
}

#[test]
fn artifact_true_with_zero_references_and_no_path_denies() {
    let state = empty_refs_session();

    let result = momus_contract("review it", &state);

    assert_eq!(result, Err(PLAN_REVIEW_DENY_MESSAGE.to_string()));
}

#[test]
fn missing_resolver_fails_closed_into_deny_for_momus() {
    // A session without plan state (no resolver) never yields a child prompt.
    let outcome = plan_review_contract_outcome(None, "momus", &format!("review {PLAN_A}"), "s1");

    assert_eq!(
        outcome,
        Some(PlanReviewContractOutcome::Deny {
            message: PLAN_REVIEW_DENY_MESSAGE.to_string(),
        })
    );
}

#[test]
fn skill_prepend_never_reaches_child_prompt() {
    let state = open_session();

    let result = momus_contract(&format!("SKILLBLOCK:: review {PLAN_A}"), &state);

    let prompt = result.expect("contract prompt");
    assert_eq!(prompt, canonical(PLAN_A));
    assert!(!prompt.contains("SKILLBLOCK"));
}

#[test]
fn prompt_substitution_depends_only_on_recorded_path() {
    let state = open_session();

    let result = momus_contract(
        &format!("review {PLAN_A} with openai/gpt-5.6-sol, round 2"),
        &state,
    );

    let prompt = result.expect("contract prompt");
    assert_eq!(prompt, canonical(PLAN_A));
    assert!(!prompt.contains("round 2"));
}

#[test]
fn batch_only_momus_item_prompt_is_forced() {
    let state = open_session();

    let momus = momus_contract("check my plan thoroughly please", &state);
    let explore = plan_review_contract_outcome(None, "explore", "scan the repo", "s1");

    assert_eq!(momus, Ok(canonical(PLAN_A)));
    assert_eq!(explore, None);
}

#[test]
fn caller_prefixed_path_uses_recorded_path_not_caller_token() {
    let state = open_session();

    let result = momus_contract(
        "review ATTACKER_CONTROLLED:.omo/plans/alpha-plan.md now",
        &state,
    );

    let prompt = result.expect("contract prompt");
    assert_eq!(prompt, canonical(PLAN_A));
    assert!(!prompt.contains("ATTACKER_CONTROLLED"));
}

#[test]
fn traversal_or_absolute_paths_fall_back_to_most_referenced_plan() {
    let state = open_session();

    let result = momus_contract(
        "review ../../secret/.omo/plans/gamma-plan.md and /tmp/untrusted/.omo/plans/delta-plan.md",
        &state,
    );

    assert_eq!(result, Ok(canonical(PLAN_A)));
}

#[test]
fn single_unrecorded_path_with_empty_references_denies() {
    let state = empty_refs_session();

    let result = momus_contract("review /tmp/untrusted/.omo/plans/delta-plan.md", &state);

    assert_eq!(result, Err(PLAN_REVIEW_DENY_MESSAGE.to_string()));
}

#[test]
fn explore_spawn_with_plan_path_is_passthrough() {
    let outcome = plan_review_contract_outcome(
        None,
        "explore",
        &format!("read {PLAN_A} and summarize"),
        "s1",
    );

    assert_eq!(outcome, None);
}

#[test]
fn equal_counts_break_ties_by_recency() {
    fn tied() -> Vec<PlanArtifactReference> {
        vec![
            PlanArtifactReference {
                path: PLAN_A.to_string(),
                count: 2,
                last_touched_at: 1,
            },
            PlanArtifactReference {
                path: PLAN_B.to_string(),
                count: 2,
                last_touched_at: 5,
            },
        ]
    }
    let state = FakeState {
        requested: vec!["ulw-plan".to_string()],
        artifact: true,
        references: tied,
    };

    let result = momus_contract("review it", &state);

    assert_eq!(result, Ok(canonical(PLAN_B)));
}

#[test]
fn extract_plan_paths_normalizes_and_dedupes() {
    let paths = extract_plan_paths(
        r"see .omo\plans\alpha-plan.md and .omo/plans/alpha-plan.md and .omo/plans/beta-plan.md",
    );

    assert_eq!(
        paths,
        vec![PLAN_A.to_string(), PLAN_B.to_string()]
    );
}

#[test]
fn fake_state_reports_session_flags() {
    let state = open_session();

    assert!(state.has_user_requested("ulw-plan"));
    assert!(!state.has_user_requested("other"));
    assert!(state.has_plan_artifact());
    assert!(!state.has_invoked("ulw-plan"));
    assert_eq!(state.plan_artifact_references().len(), 2);
}
