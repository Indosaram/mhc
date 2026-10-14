//! Subcommand execution handlers for ulw-loop CLI.

use std::fs;
use std::path::Path;
use serde_json::Value;

use crate::checkpoint_continuation::checkpoint_and_continue;
use crate::checkpoint_template::checkpoint_template;
use crate::cli_arg_parser::{
    has_flag, parse_codex_goal_json, parse_record_evidence_args, positional_text, read_stdin, read_value,
};
use crate::cli_output::{blocked_decision_handoff, normalize_codex_goal_mode, print_json, print_status};
use crate::cli_steering::{parse_steering_proposals, print_steer_batch_result, print_steer_result};
use crate::codex_goal_instruction::{build_codex_goal_instruction, BuildCodexGoalInstructionArgs};
use crate::command_types::{AddGoalOptions, CreateGoalsOptions, RecordFinalReviewBlockersOptions};
use crate::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::goal_status::is_essential_criterion;
use crate::paths::UlwLoopScope;
use crate::plan_crud::{summarize_ulw_loop_plan, StartNextResult};
use crate::plan_io::read_ulw_loop_plan;
use crate::runtime::UlwLoopError;
use crate::sdk::factory::{AgentToolkit, AgentToolkitDependencies};
use crate::sdk::types::ToolkitContext;
use crate::steering_batch::steer_ulw_loop_batch;
use crate::surface::resolve_toolkit_surface;

fn required(argv: &[String], flag: &str) -> Result<String, UlwLoopError> {
    if let Some(v) = read_value(argv, flag) {
        let trimmed = v.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    Err(UlwLoopError::new(format!("Missing {flag}."), "ULW_LOOP_ARGUMENT_MISSING"))
}

fn toolkit_for(repo_root: &Path, scope: Option<&UlwLoopScope>) -> Result<AgentToolkit, UlwLoopError> {
    let session_id = scope.and_then(|s| s.session_id.as_deref()).map(|s| s.trim()).filter(|s| !s.is_empty());
    let Some(sid) = session_id else {
        return Err(UlwLoopError::new("Missing --session-id.", "ULW_LOOP_SESSION_ID_REQUIRED"));
    };
    AgentToolkit::new(
        ToolkitContext {
            cwd: repo_root.to_string_lossy().to_string(),
            session_id: sid.to_string(),
            surface: resolve_toolkit_surface(),
        },
        None,
    )
}

pub fn create_goals(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let brief_file = read_value(argv, "--brief-file");
    let brief = if let Some(b) = read_value(argv, "--brief") {
        Some(b)
    } else if let Some(bf) = brief_file {
        Some(fs::read_to_string(&bf).map_err(|e| UlwLoopError::new(format!("Cannot read brief file: {e}"), "ULW_LOOP_BRIEF_FILE_ERROR"))?)
    } else if has_flag(argv, "--from-stdin") {
        Some(read_stdin().map_err(|e| UlwLoopError::new(format!("Cannot read stdin: {e}"), "ULW_LOOP_STDIN_ERROR"))?)
    } else {
        let pos = positional_text(argv);
        if pos.is_empty() { None } else { Some(pos) }
    };

    let Some(brief_str) = brief.filter(|s| !s.trim().is_empty()) else {
        return Err(UlwLoopError::new(
            "Missing brief text. Pass --brief, --brief-file, --from-stdin, or positional text.",
            "ULW_LOOP_BRIEF_REQUIRED",
        ));
    };

    let validation_batches_json = read_value(argv, "--validation-batch-json");
    let codex_goal_mode = normalize_codex_goal_mode(read_value(argv, "--codex-goal-mode").as_deref())?;

    let opts = CreateGoalsOptions {
        brief: brief_str,
        goals: None,
        codex_goal_mode: Some(codex_goal_mode),
        force: Some(has_flag(argv, "--force")),
        validation_batches_json,
    };

    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.create_goals(&opts);
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            let plan = result;
            if json {
                let summary = summarize_ulw_loop_plan(&plan);
                let out = serde_json::json!({
                    "ok": true,
                    "plan": plan,
                    "summary": summary,
                });
                print_json(&out);
            } else {
                println!(
                    "ulw-loop plan created: {} goal(s)\nbrief: {}\ngoals: {}\nledger: {}",
                    plan.goals.len(),
                    plan.brief_path,
                    plan.goals_path,
                    plan.ledger_path
                );
            }
            Ok(0)
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

pub fn status(repo_root: &Path, json: bool, scope: Option<&UlwLoopScope>) -> Result<i32, UlwLoopError> {
    if scope.and_then(|s| s.session_id.as_deref()).is_none() {
        return Err(UlwLoopError::new("Missing --session-id.", "ULW_LOOP_SESSION_ID_REQUIRED"));
    }
    read_ulw_loop_plan(repo_root, scope)?;
    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.status();
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            if json {
                let mut out = serde_json::json!({
                    "ok": true,
                    "plan": result.plan,
                    "summary": result.summary,
                    "nextActions": result.next_actions,
                    "evidenceRoot": result.evidence_root,
                });
                if let (Some(dir), Some(obj)) = (result.current_attempt_dir, out.as_object_mut()) {
                    obj.insert("currentAttemptDir".to_string(), Value::String(dir));
                }
                print_json(&out);
            } else {
                print_status(&result.plan);
            }
            Ok(0)
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

pub fn complete_goals(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let retry_failed = has_flag(argv, "--retry-failed");
    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.complete_goals(Some(&crate::sdk::types::CompleteGoalsArgs { retry_failed }));
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            match result {
                StartNextResult::Done { plan } => {
                    let handoff = blocked_decision_handoff(&plan);
                    if json {
                        let summary = summarize_ulw_loop_plan(&plan);
                        let out = serde_json::json!({
                            "ok": true,
                            "done": true,
                            "blocked": !handoff.is_empty(),
                            "handoff": handoff,
                            "summary": summary,
                            "plan": plan,
                        });
                        print_json(&out);
                    } else {
                        println!("{}", if handoff.is_empty() { "ulw-loop: all goals complete" } else { &handoff });
                    }
                    Ok(0)
                }
                StartNextResult::Started { plan, goal, resumed, .. } => {
                    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs {
                        plan: &plan,
                        goal: &goal,
                        is_final: None,
                        surface: None,
                    });
                    if json {
                        let out = serde_json::json!({
                            "ok": true,
                            "resumed": resumed,
                            "goal": goal,
                            "instruction": instruction,
                            "plan": plan,
                        });
                        print_json(&out);
                    } else {
                        println!("{}", instruction.text);
                    }
                    Ok(0)
                }
            }
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

pub fn steer(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let proposals = parse_steering_proposals(argv)?;
    if proposals.len() == 1 && read_value(argv, "--proposals-json").is_none() {
        let single = &proposals[0];
        let toolkit = toolkit_for(repo_root, scope)?;
        let resp = toolkit.steer(single);
        match resp {
            crate::sdk::types::ToolkitResponse::Success { result, .. } => {
                print_steer_result(&result, json);
                Ok(if result.accepted { 0 } else { 1 })
            }
            crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
                Err(UlwLoopError::new(error.message, error.code))
            }
        }
    } else {
        let result = steer_ulw_loop_batch(repo_root, &proposals, scope)?;
        print_steer_batch_result(&result, json);
        Ok(if result.accepted { 0 } else { 1 })
    }
}

pub fn add_goal(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let title = required(argv, "--title")?;
    let objective = required(argv, "--objective")?;
    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.add_goal(&AddGoalOptions {
        title,
        objective,
        evidence: None,
        success_criteria: None,
    });
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            if json {
                let summary = summarize_ulw_loop_plan(&result.plan);
                let out = serde_json::json!({
                    "ok": true,
                    "plan": result.plan,
                    "goal": result.goal,
                    "summary": summary,
                });
                print_json(&out);
            } else {
                println!("ulw-loop added goal: {}", result.goal.id);
                print_status(&result.plan);
            }
            Ok(0)
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

fn format_criterion_for_cli(criterion: &UlwLoopSuccessCriterion) -> String {
    let marker = if is_essential_criterion(criterion) {
        "essential"
    } else {
        "non-essential"
    };
    format!(
        "- {} [{}] [{marker}] ({}) {} evidence: {}",
        criterion.id,
        criterion.status,
        criterion.user_model,
        criterion.scenario,
        criterion.captured_evidence.as_deref().unwrap_or("pending")
    )
}

pub fn criteria(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let goal_id = required(argv, "--goal-id")?;
    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.criteria(&goal_id);
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            if json {
                let out = serde_json::json!({
                    "ok": true,
                    "goalId": result.goal_id,
                    "criteria": result.criteria,
                });
                print_json(&out);
            } else {
                let lines: Vec<String> = result.criteria.iter().map(format_criterion_for_cli).collect();
                println!("criteria for {}:\n{}", result.goal_id, lines.join("\n"));
            }
            Ok(0)
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

pub fn capture_evidence(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let args = parse_record_evidence_args(argv)?;
    let opts = crate::command_types::RecordEvidenceOptions {
        goal_id: args.goal_id,
        criterion_id: args.criterion_id,
        status: args.status,
        evidence: args.evidence,
        notes: args.notes,
        artifacts: None,
    };
    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.record_evidence(&opts);
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            if json {
                let summary = summarize_ulw_loop_plan(&result.plan);
                let out = serde_json::json!({
                    "ok": true,
                    "plan": result.plan,
                    "goal": result.goal,
                    "criterion": result.criterion,
                    "ledgerEntry": result.ledger_entry,
                    "summary": summary,
                });
                print_json(&out);
            } else {
                println!(
                    "ulw-loop evidence recorded: {}/{} -> {}",
                    result.goal.id, result.criterion.id, result.criterion.status
                );
            }
            Ok(0)
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

pub fn review_blockers(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    let goal_id = required(argv, "--goal-id")?;
    let title = required(argv, "--title")?;
    let objective = required(argv, "--objective")?;
    let evidence = required(argv, "--evidence")?;
    let raw_cg = required(argv, "--codex-goal-json")?;
    let codex_goal_json = parse_codex_goal_json(Some(&raw_cg))?;

    let opts = RecordFinalReviewBlockersOptions {
        goal_id,
        title,
        objective,
        evidence,
        codex_goal: None,
        codex_goal_json,
    };

    let toolkit = toolkit_for(repo_root, scope)?;
    let resp = toolkit.record_review_blockers(&opts);
    match resp {
        crate::sdk::types::ToolkitResponse::Success { result, .. } => {
            if json {
                let summary = summarize_ulw_loop_plan(&result.plan);
                let out = serde_json::json!({
                    "ok": true,
                    "plan": result.plan,
                    "blockedGoal": result.blocked_goal,
                    "goal": result.new_goal,
                    "ledgerEntries": result.ledger_entries,
                    "nextActions": result.next_actions,
                    "warnings": result.warnings,
                    "summary": summary,
                });
                print_json(&out);
            } else {
                println!(
                    "ulw-loop final review blockers recorded: {} -> review_blocked; added {}",
                    result.blocked_goal.id, result.new_goal.id
                );
            }
            Ok(0)
        }
        crate::sdk::types::ToolkitResponse::Failure { error, .. } => {
            Err(UlwLoopError::new(error.message, error.code))
        }
    }
}

pub fn checkpoint_cli(
    repo_root: &Path,
    argv: &[String],
    json: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<i32, UlwLoopError> {
    if has_flag(argv, "--print-template") {
        let goal_id = read_value(argv, "--goal-id");
        let template = checkpoint_template(repo_root, scope, goal_id.as_deref(), None)?;
        if json {
            let out = serde_json::json!({
                "ok": true,
                "qualityGateTemplate": template.quality_gate_template,
                "codexGoalTemplate": template.codex_goal_template,
                "guidance": template.guidance,
                "attemptDir": template.attempt_dir,
            });
            print_json(&out);
        } else {
            print_json(&template);
        }
        return Ok(0);
    }

    let goal_id = required(argv, "--goal-id")?;
    let raw_status = required(argv, "--status")?;
    let status_val = match raw_status.as_str() {
        "complete" | "failed" | "blocked" => raw_status,
        _ => {
            return Err(UlwLoopError::new(
                "Missing or invalid --status; expected complete, failed, or blocked.",
                "ULW_LOOP_STATUS_INVALID",
            ));
        }
    };
    let evidence = required(argv, "--evidence")?;
    let codex_goal_json = parse_codex_goal_json(read_value(argv, "--codex-goal-json").as_deref())?;
    let quality_gate_json = read_value(argv, "--quality-gate-json");
    let advance = !has_flag(argv, "--no-advance");

    let args = crate::checkpoint::CheckpointUlwLoopArgs {
        goal_id,
        status: status_val,
        evidence,
        codex_goal_json,
        quality_gate_json,
    };

    let result = checkpoint_and_continue(repo_root, &args, advance, scope)?;
    if json {
        let summary = summarize_ulw_loop_plan(&result.checkpoint.plan);
        let out = serde_json::json!({
            "ok": true,
            "plan": result.checkpoint.plan,
            "goal": result.checkpoint.goal,
            "ledgerEntry": result.checkpoint.ledger_entry,
            "aggregateCompletion": result.checkpoint.aggregate_completion,
            "nextActions": result.checkpoint.next_actions,
            "warnings": result.checkpoint.warnings,
            "next": result.next,
            "summary": summary,
        });
        print_json(&out);
    } else {
        println!("ulw-loop checkpoint: {} -> {}", result.checkpoint.goal.id, result.checkpoint.goal.status);
        if let Some(next) = &result.next {
            match next {
                crate::checkpoint_continuation::ContinuationNext::Resumed { instruction, .. } => {
                    println!("{}", instruction.text);
                }
                crate::checkpoint_continuation::ContinuationNext::Done { handoff, .. } => {
                    println!("{}", if handoff.is_empty() { "ulw-loop: all goals complete" } else { handoff });
                }
            }
        }
    }
    Ok(0)
}
