//! `depth-policy.test.ts`, `execution-mode.test.ts`, `continue-result.test.ts`.

use crate::agents::interaction_policy_for_agent;
use crate::manager::continue_result::{CONTINUE_SUGGESTION, ContinueResult, to_continue_result};
use crate::manager::depth_policy::{DepthDecision, DepthPolicyInput, decide_depth_policy};
use crate::manager::execution_mode::{ExecutionMode, ExecutionModeSources, resolve_execution_mode};
use crate::steering::SendOutcome;

fn depth(
    child_depth: u32,
    max_depth: u32,
    target: Option<&str>,
    allowed: Option<&[String]>,
) -> DepthDecision {
    decide_depth_policy(&DepthPolicyInput {
        child_depth,
        max_depth,
        target_agent_type: target,
        allowed_subagents: allowed,
    })
}

#[test]
fn given_a_top_level_child_within_max_depth_when_decided_then_it_is_allowed() {
    assert!(depth(1, 1, None, None).allowed());
}

#[test]
fn given_a_child_beyond_max_depth_with_no_allowance_when_decided_then_it_is_denied() {
    let DepthDecision::Denied { reason } = depth(2, 1, None, None) else {
        panic!("expected denial");
    };
    assert!(reason.contains('2'));
    assert!(reason.contains('1'));
}

#[test]
fn given_a_deeper_child_whose_type_is_in_allowed_subagents_when_decided_then_it_is_allowed() {
    let allowed = ["explorer".to_string()];
    assert_eq!(
        depth(3, 1, Some("explorer"), Some(&allowed)),
        DepthDecision::AllowedSubagent
    );
}

#[test]
fn given_a_deeper_child_whose_type_is_not_in_allowed_subagents_when_decided_then_it_is_denied() {
    let allowed = ["explorer".to_string()];
    assert!(!depth(2, 1, Some("writer"), Some(&allowed)).allowed());
}

#[test]
fn given_a_spec_mode_when_resolved_then_the_spec_mode_wins_over_every_other_source() {
    let mode = resolve_execution_mode(ExecutionModeSources {
        spec_mode: Some(ExecutionMode::Process),
        agent_mode: Some(ExecutionMode::InProcess),
        config_mode: Some(ExecutionMode::InProcess),
    });
    assert_eq!(mode, ExecutionMode::Process);
}

#[test]
fn given_no_spec_mode_but_an_agent_mode_when_resolved_then_the_agent_mode_wins_over_config() {
    let mode = resolve_execution_mode(ExecutionModeSources {
        agent_mode: Some(ExecutionMode::Process),
        config_mode: Some(ExecutionMode::InProcess),
        ..ExecutionModeSources::default()
    });
    assert_eq!(mode, ExecutionMode::Process);
}

#[test]
fn given_only_a_config_mode_when_resolved_then_the_config_mode_is_used() {
    let mode = resolve_execution_mode(ExecutionModeSources {
        config_mode: Some(ExecutionMode::Process),
        ..ExecutionModeSources::default()
    });
    assert_eq!(mode, ExecutionMode::Process);
}

#[test]
fn given_no_source_at_all_when_resolved_then_it_falls_back_to_in_process() {
    assert_eq!(
        resolve_execution_mode(ExecutionModeSources::default()),
        ExecutionMode::InProcess
    );
}

#[test]
fn given_a_one_shot_agent_send_outcome_when_adapted_then_it_is_not_continuable_with_the_registry_reminder_as_the_reason()
 {
    let reminder = interaction_policy_for_agent("momus")
        .expect("momus policy")
        .send_denial_reminder;
    let result = to_continue_result(SendOutcome::OneShotAgent {
        task_id: "st_00000001".to_string(),
        agent: "momus".to_string(),
        message: reminder.to_string(),
    });
    assert_eq!(
        result,
        ContinueResult::NotContinuable {
            task_id: Some("st_00000001".to_string()),
            reason: reminder.to_string(),
            suggestion: CONTINUE_SUGGESTION.to_string(),
        }
    );
}
