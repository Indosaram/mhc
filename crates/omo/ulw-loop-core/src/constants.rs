//! Constants and domain string enumerations for ulw-loop.

pub const ULW_LOOP_DIR: &str = ".omo/ulw-loop";
pub const ULW_LOOP_BRIEF: &str = "brief.md";
pub const ULW_LOOP_GOALS: &str = "goals.json";
pub const ULW_LOOP_LEDGER: &str = "ledger.jsonl";
pub const ULW_LOOP_STATE_LOCK: &str = ".state.lock";

pub const ULW_LOOP_STEERING_MUTATION_KINDS: &[&str] = &[
    "add_subgoal",
    "split_subgoal",
    "reorder_pending",
    "revise_pending_wording",
    "revise_criterion",
    "annotate_ledger",
    "mark_blocked_superseded",
];

pub const ULW_LOOP_SUCCESS_CRITERION_USER_MODELS: &[&str] = &[
    "happy",
    "edge",
    "regression",
    "adversarial",
];

pub const ULW_LOOP_CRITERION_STATUSES: &[&str] = &["pending", "pass", "fail", "blocked"];

pub const ULW_LOOP_LEDGER_EVENT_KINDS: &[&str] = &[
    "plan_created",
    "goal_started",
    "goal_resumed",
    "goal_completed",
    "goal_blocked",
    "goal_failed",
    "goal_needs_user_decision",
    "goal_retried",
    "aggregate_completed",
    "aggregate_objective_migrated",
    "goal_added",
    "steering_accepted",
    "steering_rejected",
    "final_review_failed",
    "goal_review_blocked",
    "evidence_captured",
    "criterion_failed",
    "criterion_blocked",
    "criteria_revised",
    "batch_closed",
    "batch_updated",
];
