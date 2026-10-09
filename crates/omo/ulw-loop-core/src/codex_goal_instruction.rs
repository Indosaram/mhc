//! Codex goal instruction generation for agent consumption.

use serde::{Deserialize, Serialize};

use crate::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::goal_status::{codex_goal_mode, expected_codex_objective, is_essential_criterion, is_final_run_completion_candidate};
use crate::surface::{resolve_toolkit_surface, reviewer_roles_for, UlwLoopToolkitSurface};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexCreateGoalPayload {
    pub objective: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UlwLoopGoalInstruction {
    pub text: String,
    pub json: CodexCreateGoalPayload,
}

pub struct BuildCodexGoalInstructionArgs<'a> {
    pub plan: &'a UlwLoopPlan,
    pub goal: &'a UlwLoopItem,
    pub is_final: Option<bool>,
    pub surface: Option<UlwLoopToolkitSurface>,
}

pub fn build_codex_goal_instruction(args: BuildCodexGoalInstructionArgs) -> UlwLoopGoalInstruction {
    let mode = codex_goal_mode(args.plan);
    let create_goal = build_create_goal_payload(args.plan, args.goal);
    let is_final = args
        .is_final
        .unwrap_or_else(|| is_final_run_completion_candidate(args.plan, args.goal));
    let surface = args.surface.unwrap_or_else(resolve_toolkit_surface);
    let text = build_text(mode, args.plan, args.goal, &create_goal, is_final, surface);
    UlwLoopGoalInstruction { text, json: create_goal }
}

fn build_create_goal_payload(plan: &UlwLoopPlan, goal: &UlwLoopItem) -> CodexCreateGoalPayload {
    CodexCreateGoalPayload {
        objective: expected_codex_objective(plan, goal),
    }
}

fn build_text(
    mode: &str,
    plan: &UlwLoopPlan,
    goal: &UlwLoopItem,
    create_goal: &CodexCreateGoalPayload,
    is_final: bool,
    surface: UlwLoopToolkitSurface,
) -> String {
    let header = if mode == "aggregate" {
        "UlwLoop aggregate-goal handoff"
    } else {
        "UlwLoop active-goal handoff"
    };

    let mut lines = vec![
        header.to_string(),
        format!("Mode: {mode}"),
        format!("Plan: {}", plan.goals_path),
        format!("Ledger: {}", plan.ledger_path),
        format!("Goal: {} — {}", goal.id, goal.title),
        String::new(),
    ];

    lines.extend(active_goal_lines(goal));
    lines.push(String::new());
    lines.extend(success_criteria_lines(&goal.success_criteria));
    lines.push(String::new());
    lines.push("Codex goal integration constraints:".to_string());
    lines.push("- Use the create_goal payload exactly as rendered: objective only.".to_string());
    lines.push("- Goals are unlimited. Do not add numeric limits.".to_string());
    lines.extend(mode_constraint_lines(mode, is_final));
    lines.extend(evidence_layout_lines(plan));
    lines.push(final_section(plan, goal, is_final, mode == "aggregate", surface));
    lines.extend(checkpoint_lines(plan, mode));
    lines.push(String::new());
    lines.push("create_goal payload:".to_string());
    lines.push(serde_json::to_string_pretty(create_goal).unwrap_or_default());

    lines.join("\n")
}

fn mode_constraint_lines(mode: &str, is_final: bool) -> Vec<String> {
    if mode == "per_story" {
        return vec![
            "- First call get_goal. If no active goal exists, call create_goal with the payload below.".into(),
            "- If a different active Codex goal exists, finish/checkpoint that goal before starting this ulw-loop.".into(),
            "- Work only this goal until its completion audit passes.".into(),
        ];
    }
    vec![
        "- Codex goal = the whole ulw-loop run; OMO G001/G002/etc. = ledger stories.".into(),
        "- First call get_goal. If no active goal exists, call create_goal with the aggregate payload below.".into(),
        "- If get_goal reports the same aggregate objective as active, continue this OMO story without creating a new Codex goal.".into(),
        "- If a different active or incomplete Codex goal exists, finish/checkpoint that goal before starting this ulw-loop.".into(),
        if is_final {
            "- This is the final story; update_goal is allowed only after the mandatory quality gate passes.".into()
        } else {
            "- This is not the final story: do not call update_goal mid-aggregate; checkpoint this OMO ledger story and continue the remaining stories. update_goal is reserved for the final story after the mandatory quality gate passes.".into()
        },
    ]
}

fn checkpoint_lines(plan: &UlwLoopPlan, mode: &str) -> Vec<String> {
    let failure_line = format!(
        "- If blocked or failed, checkpoint with --status failed and the failure evidence; rerun complete-goals{} --retry-failed to resume.",
        session_option(plan)
    );
    if mode == "per_story" {
        return vec![failure_line];
    }
    vec![
        "- Checkpoint this OMO story with a fresh get_goal snapshot whose objective matches the aggregate payload.".into(),
        failure_line,
    ]
}

fn active_goal_lines(goal: &UlwLoopItem) -> Vec<String> {
    vec![
        "Active goal:".into(),
        format!("- id: {}", goal.id),
        format!("- title: {}", goal.title),
        format!("- objective: {}", goal.objective),
    ]
}

fn success_criteria_lines(criteria: &[UlwLoopSuccessCriterion]) -> Vec<String> {
    if criteria.is_empty() {
        return vec![
            "Success criteria:".into(),
            "- No success criteria recorded for this goal.".into(),
        ];
    }
    let mut lines = vec!["Success criteria:".to_string()];
    for c in criteria {
        lines.push(format_criterion_line(c));
    }
    lines
}

fn format_criterion_line(criterion: &UlwLoopSuccessCriterion) -> String {
    let remaining_work = if criterion.status == "pending" {
        " remaining work:"
    } else {
        ""
    };
    let marker = if is_essential_criterion(criterion) {
        "essential"
    } else {
        "non-essential"
    };
    format!(
        "-{remaining_work} [{}] [{marker}] ({}) {} — expect: {} — status: {}",
        criterion.id, criterion.user_model, criterion.scenario, criterion.expected_evidence, criterion.status
    )
}

fn evidence_layout_lines(plan: &UlwLoopPlan) -> Vec<String> {
    if plan.evidence_layout_version != Some(2) {
        return Vec::new();
    }
    vec![
        "- Evidence layout v2: write every artifact for the active goal (QA matrix, review reports, receipts) under the current attempt directory — read currentAttemptDir from `ulw-loop status --json` (.omo/evidence/ulw/<session>/<goalId>/a<attempt>). The final checkpoint rejects quality-gate artifacts outside that directory.".into()
    ]
}

fn final_section(
    plan: &UlwLoopPlan,
    goal: &UlwLoopItem,
    is_final: bool,
    aggregate: bool,
    surface: UlwLoopToolkitSurface,
) -> String {
    let roles = reviewer_roles_for(surface);
    if !is_final {
        return "- This is not the final ulw-loop story; do not run the final reviewer/manual-QA/gate-review quality gate yet.".into();
    }
    let option = session_option(plan);
    if surface == UlwLoopToolkitSurface::OmoSenpi {
        return senpi_final_section(plan, goal, aggregate);
    }

    let blocker_cmd = format!(
        "ulw-loop record-review-blockers{option} --goal-id {} --title \"Resolve final code-review blockers\" --objective \"<blocker-resolution objective>\" --evidence \"<review findings>\" --codex-goal-json \"<active get_goal JSON or path>\"",
        goal.id
    );
    let checkpoint_cmd = format!(
        "ulw-loop checkpoint{option} --goal-id {} --status complete --evidence \"<targeted verification/manualQa/gateReview evidence>\" --codex-goal-json \"<fresh complete get_goal JSON or path>\" --quality-gate-json \"<quality gate JSON or path>\"",
        goal.id
    );

    vec![
        "Final story — self-review and manual QA are the default; use the quality gate before update_goal:".into(),
        "- Run targeted verification for changed behavior.".into(),
        "- Confirm every manualQa artifact path exists and has non-zero size.".into(),
        format!(
            "- Run manual QA yourself and write its artifact under currentAttemptDir. Only if the user explicitly demands strict, rigorous, or high-accuracy review, spawn {}, optionally also {} and {}; otherwise set manualQa.by and gateReview.by to \"main-session\".",
            roles.gate_review, roles.code_review, roles.manual_qa
        ),
        "- Require passed manualQa, approved gateReview, passed iteration, and complete criteriaCoverage; include codeReview only when strict review was requested. criteriaCoverage must summarize originalIntent, desiredOutcome, and userOutcomeReview; counts alone are not approval.".into(),
        "- On a reviewer REJECT, fix only the cited blockers, rerun the affected verification/Manual-QA, and re-review the delta at most TWICE; if blockers remain, record them and surface to the user.".into(),
        "- If codeQualityStatus is WATCH, include the WATCH notes verbatim in your final user-facing message.".into(),
        "- If any reviewer is blocked/inconclusive or the quality gate is not clean, do not call update_goal. Record blocker work first:".into(),
        format!("  {blocker_cmd}"),
        if aggregate {
            "- If the quality gate is clean, call update_goal({status: \"complete\"}), call get_goal again, then checkpoint the aggregate story:".into()
        } else {
            "- If the quality gate is clean, call update_goal({status: \"complete\"}), call get_goal again, then checkpoint:".into()
        },
        format!("  {checkpoint_cmd}"),
    ].join("\n")
}

fn senpi_final_section(plan: &UlwLoopPlan, goal: &UlwLoopItem, aggregate: bool) -> String {
    let option = session_option(plan);
    let checkpoint_cmd = format!(
        "ulw-loop checkpoint{option} --goal-id {} --status complete --evidence \"<manualQa/gateReview evidence>\" --codex-goal-json \"<fresh complete get_goal JSON or path>\" --quality-gate-json \"$(ulw-loop checkpoint{option} --print-template)\"",
        goal.id
    );

    vec![
        "Final story — run the single-reviewer quality gate before update_goal:".into(),
        "- Run manual QA yourself and write the non-empty artifact under currentAttemptDir; set manualQa.by to \"main-session\".".into(),
        "- Spawn exactly one gate reviewer with task(category: \"deep-high\").".into(),
        "- If that task fails with any model_unavailable failure, retry with category:deep-low, then category:unspecified-high, then category:unspecified-low.".into(),
        "- Set gateReview.by to the exact category:<name> literal used for the successful task.".into(),
        "- Build the gate JSON with ulw-loop checkpoint --print-template, then fill manualQa, gateReview, iteration, and criteriaCoverage.".into(),
        "- Require passed manualQa, approved gateReview, passed iteration, and complete criteriaCoverage before update_goal({status: \"complete\"}).".into(),
        if aggregate {
            format!("- If the gate is clean, call update_goal({{status: \"complete\"}}), call get_goal again, then checkpoint the aggregate story: {checkpoint_cmd}")
        } else {
            format!("- If the gate is clean, call update_goal({{status: \"complete\"}}), call get_goal again, then checkpoint: {checkpoint_cmd}")
        },
    ].join("\n")
}

fn session_option(plan: &UlwLoopPlan) -> String {
    let prefix = ".omo/ulw-loop/";
    let suffix = "/goals.json";
    if !plan.goals_path.starts_with(prefix) || !plan.goals_path.ends_with(suffix) {
        return String::new();
    }
    if plan.goals_path.len() < prefix.len() + suffix.len() {
        return String::new();
    }
    let session_id = &plan.goals_path[prefix.len()..plan.goals_path.len() - suffix.len()];
    if session_id.is_empty() {
        String::new()
    } else {
        format!(" --session-id {session_id}")
    }
}
