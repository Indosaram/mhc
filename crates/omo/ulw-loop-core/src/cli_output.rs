//! Output formatting for ulw-loop CLI.

use serde_json::Value;
use crate::domain_types::{UlwLoopItem, UlwLoopPlan};
use crate::runtime::UlwLoopError;

pub const ULW_LOOP_HELP: &str = "Usage:\n\
  ulw-loop hook user-prompt-submit [--with-ultrawork]  (Codex UserPromptSubmit hook)\n\
  ulw-loop help | --help | -h                          (this message)\n\
  ulw-loop ulw-loop help\n\
  ulw-loop ulw-loop create-goals --brief \"...\" [--brief-file <path>] [--from-stdin] [--codex-goal-mode aggregate|per_story] [--validation-batch-json <json-or-path>] [--force] [--json]\n\
  ulw-loop ulw-loop status [--json]\n\
  ulw-loop ulw-loop complete-goals [--retry-failed] [--json]\n\
  ulw-loop ulw-loop criteria --goal-id <id> [--json]\n\
  ulw-loop ulw-loop record-evidence --goal-id <id> --criterion-id <id> --status pass|fail|blocked --evidence \"...\" [--notes \"...\"] [--json]\n\
  ulw-loop ulw-loop checkpoint --print-template [--goal-id <id>] [--json]\n\
  ulw-loop ulw-loop checkpoint --goal-id <id> --status complete|failed|blocked --evidence \"...\" --codex-goal-json <...> [--quality-gate-json <...>] [--no-advance] [--json]\n\
  ulw-loop ulw-loop steer --kind <kind> ... --evidence \"...\" --rationale \"...\" [--proposals-json <json-or-path>] [--json]\n\
  ulw-loop ulw-loop add-goal --title \"...\" --objective \"...\" [--json]\n\
  ulw-loop ulw-loop record-review-blockers --goal-id <id> --title \"...\" --objective \"...\" --evidence \"...\" --codex-goal-json <...> [--json]\n\
\n\
Every state subcommand needs a session scope: [--session-id <id>] or the session env (OMO_ULW_LOOP_SESSION_ID / CODEX_SESSION_ID / CODEX_THREAD_ID / PI_SESSION_ID); state lives under .omo/ulw-loop/<id>/ and the unscoped root is never used implicitly. status --json exposes the currentAttemptDir; put all quality-gate artifacts under it.\n\
Every subcommand accepts --help | -h to print its own usage line.";

pub fn subcommand_help(subcommand: &str) -> String {
    let prefix = format!("ulw-loop ulw-loop {subcommand}");
    let lines: Vec<&str> = ULW_LOOP_HELP
        .lines()
        .filter(|line| line.trim_start().starts_with(&prefix))
        .collect();
    if lines.is_empty() {
        ULW_LOOP_HELP.to_string()
    } else {
        format!("Usage:\n{}", lines.join("\n"))
    }
}

pub fn print_json<T: serde::Serialize>(value: &T) {
    if let Ok(s) = serde_json::to_string_pretty(value) {
        println!("{s}");
    }
}

pub fn print_json_error(error: &UlwLoopError) {
    let mut err_obj = serde_json::json!({
        "code": error.code,
        "message": error.message,
    });
    if let Some(details) = &error.details {
        err_obj["details"] = serde_json::to_value(details).unwrap_or(Value::Null);
    }
    let out = serde_json::json!({
        "ok": false,
        "error": err_obj,
    });
    print_json(&out);
}

fn criteria_counts(goal: &UlwLoopItem) -> (usize, usize) {
    let mut pass = 0;
    for c in &goal.success_criteria {
        if c.status == "pass" {
            pass += 1;
        }
    }
    (pass, goal.success_criteria.len())
}

pub fn print_status(plan: &UlwLoopPlan) {
    let mut total_criteria = 0;
    let mut pass_criteria = 0;
    let mut lines = vec!["ulw-loop status".to_string(), String::new(), "goals:".to_string()];

    for goal in &plan.goals {
        let (pass, total) = criteria_counts(goal);
        total_criteria += total;
        pass_criteria += pass;
        let marker = if plan.active_goal_id.as_deref() == Some(&goal.id) {
            "*"
        } else {
            "-"
        };
        lines.push(format!("{marker} {} [{}] {} (criteria: {pass}/{total})", goal.id, goal.status, goal.title));
    }

    lines.push(String::new());
    lines.push("summary:".to_string());
    lines.push(format!("total goals: {}", plan.goals.len()));
    lines.push(format!("criteria: {pass_criteria}/{total_criteria} pass"));
    println!("{}", lines.join("\n"));
}

pub fn blocked_decision_handoff(plan: &UlwLoopPlan) -> String {
    let blocked = plan.goals.iter().find(|g| g.status == "needs_user_decision" && g.non_retriable == Some(true));
    let Some(blocked) = blocked else {
        return String::new();
    };

    let req = blocked.required_external_decision.as_deref().unwrap_or("provide the missing authorization or choose a different unblock path");
    vec![
        "ulw-loop: blocked on repeated external authorization; no retryable failed goals remain.".to_string(),
        format!("Goal: {} - {}", blocked.id, blocked.title),
        format!("Required external decision: {req}."),
        "Do not run complete-goals --retry-failed again until external state changes or the user authorizes an unblock path.".to_string(),
    ].join("\n")
}

pub fn normalize_codex_goal_mode(value: Option<&str>) -> Result<String, UlwLoopError> {
    match value {
        None | Some("aggregate") => Ok("aggregate".to_string()),
        Some("per_story") => Ok("per_story".to_string()),
        Some(v) => Err(UlwLoopError::new(
            "Invalid --codex-goal-mode; expected aggregate or per_story.",
            "ULW_LOOP_CODEX_GOAL_MODE_INVALID",
        )),
    }
}
