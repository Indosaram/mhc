//! The `workflow` tool (spec §4): parameter parsing, action dispatch and result shapes. Every
//! result has a short text line and details whose `kind` names the action family, or `error`
//! with `{code, message, errors, diagnostics, node_ids}`.
use crate::definition::WorkflowDefinition;
use crate::executor::SendOutcome;
use crate::scheduler::{WorkflowError, WorkflowScheduler};
use crate::scheduler_ops::GuideOp;
use maho_task::Notifier;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_NAME: &str = "workflow";
/// `custom_type` of workflow notifications.
pub const UPDATE_MESSAGE_TYPE: &str = crate::scheduler::NOTICE_TYPE;

/// Action values (interface facts).
pub const ACTIONS: &[&str] = &[
    "start", "attach", "snapshot", "wait", "cancel", "retry", "send", "amend", "guide",
    "cancel_node", "resume",
];

/// The calling session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub session_id: String,
    pub root_session: Option<String>,
}

impl Caller {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            root_session: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutcome {
    pub text: String,
    pub details: Value,
    pub is_error: bool,
}

impl ToolOutcome {
    fn ok(text: String, details: Value) -> Self {
        Self { text, details, is_error: false }
    }

    pub fn error(error: &WorkflowError) -> Self {
        Self {
            text: format!("{}: {}", error.code, error.message),
            details: json!({
                "kind": "error",
                "code": error.code,
                "message": error.message,
                "errors": error.errors,
                "diagnostics": error.diagnostics,
                "node_ids": error.node_ids,
            }),
            is_error: true,
        }
    }

    /// The error code when this is an error result.
    pub fn code(&self) -> Option<&str> {
        self.is_error.then(|| self.details.get("code").and_then(Value::as_str)).flatten()
    }
}

fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn definition_param(params: &Value) -> Result<WorkflowDefinition, WorkflowError> {
    let raw = params
        .get("definition")
        .ok_or_else(|| WorkflowError::new("invalid_arguments", "this action needs `definition`"))?;
    serde_json::from_value(raw.clone()).map_err(|error| {
        WorkflowError::new("invalid_definition", format!("the definition does not have the expected shape: {error}"))
    })
}

fn snapshot_json(scheduler: &WorkflowScheduler, run: &crate::state::WorkflowRun) -> Value {
    serde_json::to_value(scheduler.snapshot_of(run)).unwrap_or(Value::Null)
}

fn status_line(run: &crate::state::WorkflowRun) -> String {
    let (done, total, running, failed) = run.counts();
    let mut line = format!("{} ({}) {}: {done}/{total} done", run.name, run.run_id, run.status.as_str());
    if running > 0 {
        line.push_str(&format!(", {running} running"));
    }
    if failed > 0 {
        line.push_str(&format!(", {failed} failed"));
    }
    line
}

/// JSON schema of the tool parameters.
pub fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ACTIONS},
            "definition": {
                "type": "object",
                "description": "Workflow definition {key, name, nodes[]}; each node {id, prompt, label?, description?, task_summary?, dependsOn?, load_skills?, category | subagent_type, model? (with subagent_type only)}",
                "properties": {
                    "key": {"type": "string", "description": "Idempotency key, unique in this session"},
                    "name": {"type": "string"},
                    "nodes": {"type": "array", "items": {"type": "object", "properties": {
                        "id": {"type": "string"},
                        "prompt": {"type": "string"},
                        "label": {"type": "string"},
                        "description": {"type": "string"},
                        "task_summary": {"type": "string"},
                        "dependsOn": {"type": "array", "items": {"type": "string"}},
                        "load_skills": {"type": "array", "items": {"type": "string"}},
                        "category": {"type": "string"},
                        "subagent_type": {"type": "string"},
                        "model": {"type": "string"}
                    }, "required": ["id", "prompt"]}}
                },
                "required": ["key", "name", "nodes"]
            },
            "run_id": {"type": "string"},
            "reason": {"type": "string"},
            "node_id": {"type": "string"},
            "node_ids": {"type": "array", "items": {"type": "string"}},
            "message": {"type": "string"},
            "prompt": {"type": "string"},
            "detach": {"type": "boolean", "description": "wait: return at once and get notified (default true)"},
            "guidance_id": {"type": "string"},
            "text": {"type": "string"},
            "remove": {"type": "boolean"}
        },
        "required": ["action"]
    })
}

pub const DESCRIPTION: &str = "Run a dependency graph of delegated tasks. Each node is one subagent task (`category` or `subagent_type`); `dependsOn` only orders nodes, it passes no data, so every prompt must stand alone (later nodes read earlier results from the workspace). \
Nodes whose dependencies completed start in parallel; a failed node skips its dependents but not unrelated branches. \
Actions: start (definition; same key + same definition reuses the run), attach, snapshot, wait (detach defaults to true: you are notified per settled node and when the run settles; detach=false blocks), cancel, retry (settled run; failed/cancelled/skipped nodes or node_id/node_ids, prompt only with one node), send (message to a node's child), amend (same key; changed nodes and their dependents re-run), guide / cancel_node (runs the user stopped), resume.";

/// Runs one tool call.
pub async fn run(scheduler: &Arc<WorkflowScheduler>, caller: &Caller, params: &Value, notifier: Option<Notifier>) -> ToolOutcome {
    match dispatch(scheduler, caller, params, notifier).await {
        Ok(outcome) => outcome,
        Err(error) => ToolOutcome::error(&error),
    }
}

async fn dispatch(scheduler: &Arc<WorkflowScheduler>, caller: &Caller, params: &Value, notifier: Option<Notifier>) -> Result<ToolOutcome, WorkflowError> {
    let action = str_param(params, "action").ok_or_else(|| WorkflowError::new("invalid_arguments", "`action` is required"))?;
    let run_id = str_param(params, "run_id");
    match action {
        "start" => {
            let definition = definition_param(params)?;
            let outcome = scheduler.start(caller, definition)?;
            let run = &outcome.run;
            let text = if outcome.reused {
                format!("Reused workflow {}", status_line(run))
            } else {
                format!("Started workflow {}", status_line(run))
            };
            Ok(ToolOutcome::ok(text, json!({
                "kind": "started",
                "run_id": run.run_id,
                "reused": outcome.reused,
                "snapshot": snapshot_json(scheduler, run),
                "warnings": outcome.warnings,
                "diagnostics": outcome.diagnostics,
            })))
        }
        "attach" => {
            let (snapshot, active) = scheduler.attach(caller, run_id, notifier)?;
            let text = if active {
                format!("Attached to workflow {} ({}); this session is notified when it settles.", snapshot.name, snapshot.run_id)
            } else {
                format!("Workflow {} ({}) already {}.", snapshot.name, snapshot.run_id, snapshot.status.as_str())
            };
            Ok(ToolOutcome::ok(text, json!({"kind": "attached", "run_id": snapshot.run_id, "live": active, "snapshot": snapshot})))
        }
        "snapshot" => {
            let snapshot = scheduler.snapshot(caller, run_id)?;
            let mut text = format!("Workflow {} ({}) {}", snapshot.name, snapshot.run_id, snapshot.status.as_str());
            if !snapshot.stalled.is_empty() {
                text.push_str(&format!("; no child activity for 10 minutes: {}", snapshot.stalled.join(", ")));
            }
            Ok(ToolOutcome::ok(text, json!({"kind": "snapshot", "run_id": snapshot.run_id, "snapshot": snapshot})))
        }
        "wait" => {
            let detach = params.get("detach").and_then(Value::as_bool).unwrap_or(true);
            if detach {
                let pending = match notifier {
                    Some(notifier) => scheduler.wait_detached(caller, run_id, notifier)?,
                    None => false,
                };
                if pending {
                    let snapshot = scheduler.snapshot(caller, run_id)?;
                    return Ok(ToolOutcome::ok(
                        format!("Waiting for workflow {} ({}) in the background; you get a message per settled node and one when the run settles.", snapshot.name, snapshot.run_id),
                        json!({"kind": "detached", "run_id": snapshot.run_id, "snapshot": snapshot}),
                    ));
                }
                let live = scheduler.access(caller, run_id)?;
                let run = crate::lock(&live.run).clone();
                if !run.status.is_terminal() {
                    // No notifier to wake this session later: block instead.
                    return waited(scheduler, caller, run_id).await;
                }
                return Ok(ToolOutcome::ok(
                    format!("Workflow {}", status_line(&run)),
                    json!({"kind": "waited", "run_id": run.run_id, "status": run.status.as_str(), "results": scheduler.results_of(&run)}),
                ));
            }
            waited(scheduler, caller, run_id).await
        }
        "cancel" => {
            let run = scheduler.cancel(caller, run_id, str_param(params, "reason"))?;
            Ok(ToolOutcome::ok(format!("Workflow {}", status_line(&run)), json!({"kind": "cancelled", "run_id": run.run_id, "status": run.status.as_str()})))
        }
        "retry" => {
            let node_ids = match (str_param(params, "node_id"), params.get("node_ids").and_then(Value::as_array)) {
                (Some(_), Some(_)) => return Err(WorkflowError::new("invalid_arguments", "pass `node_id` or `node_ids`, not both")),
                (Some(id), None) => Some(vec![id.to_owned()]),
                (None, Some(ids)) => Some(ids.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
                (None, None) => None,
            };
            let (run, selected) = scheduler.retry(caller, run_id, node_ids, str_param(params, "prompt"))?;
            Ok(ToolOutcome::ok(
                format!("Retrying {} node(s) of workflow {} ({})", selected.len(), run.name, run.run_id),
                json!({"kind": "retried", "run_id": run.run_id, "node_ids": selected, "snapshot": snapshot_json(scheduler, &run)}),
            ))
        }
        "send" => {
            let outcome = scheduler.send(caller, run_id, str_param(params, "node_id"), str_param(params, "message"))?;
            let how = match outcome {
                SendOutcome::Steered => "steered",
                _ => "follow_up",
            };
            Ok(ToolOutcome::ok(format!("Message delivered to node {} ({how}).", str_param(params, "node_id").unwrap_or_default()), json!({"kind": "sent", "delivery": how})))
        }
        "amend" => {
            let definition = definition_param(params)?;
            let (run, rerun) = scheduler.amend(caller, run_id, definition)?;
            Ok(ToolOutcome::ok(
                format!("Amended workflow {} ({}); {} node(s) will run again", run.name, run.run_id, rerun.len()),
                json!({"kind": "amended", "run_id": run.run_id, "node_ids": rerun, "amendments": run.amendments, "snapshot": snapshot_json(scheduler, &run)}),
            ))
        }
        "guide" => {
            let text = params.get("text").and_then(Value::as_str).map(str::to_owned);
            let op = match (str_param(params, "guidance_id"), params.get("remove").and_then(Value::as_bool).unwrap_or(false)) {
                (Some(id), true) => GuideOp::Remove { guidance_id: id.to_owned() },
                (Some(id), false) => GuideOp::Edit {
                    guidance_id: id.to_owned(),
                    text: text.ok_or_else(|| WorkflowError::new("invalid_arguments", "editing guidance needs `text`"))?,
                },
                (None, true) => return Err(WorkflowError::new("invalid_arguments", "removing guidance needs `guidance_id`")),
                (None, false) => GuideOp::Add {
                    text: text.ok_or_else(|| WorkflowError::new("invalid_arguments", "adding guidance needs `text`"))?,
                    node_id: str_param(params, "node_id").map(str::to_owned),
                },
            };
            let id = scheduler.guide(caller, run_id, op)?;
            // The spec gives guide no result kind of its own; it reports the run snapshot.
            let snapshot = scheduler.snapshot(caller, run_id)?;
            Ok(ToolOutcome::ok(format!("Guidance {id} updated; it is delivered when the user resumes the run."), json!({"kind": "snapshot", "guidance_id": id, "run_id": snapshot.run_id, "snapshot": snapshot})))
        }
        "cancel_node" => {
            let run = scheduler.cancel_node(caller, run_id, str_param(params, "node_id"))?;
            Ok(ToolOutcome::ok(
                format!("Cancelled node {}; workflow {} stays {}", str_param(params, "node_id").unwrap_or_default(), run.run_id, run.status.as_str()),
                json!({"kind": "cancelled", "run_id": run.run_id, "node_ids": [str_param(params, "node_id")], "status": run.status.as_str()}),
            ))
        }
        "resume" => {
            scheduler.resume_action(caller, run_id)?;
            Err(WorkflowError::new("run_not_user_paused", "nothing to resume"))
        }
        other => Err(WorkflowError::new("invalid_arguments", format!("unknown action \"{other}\" (one of: {})", ACTIONS.join(", ")))),
    }
}

async fn waited(scheduler: &Arc<WorkflowScheduler>, caller: &Caller, run_id: Option<&str>) -> Result<ToolOutcome, WorkflowError> {
    let (run, results) = scheduler.wait_settled(caller, run_id).await?;
    let mut lines = vec![format!("Workflow {}", status_line(&run))];
    for result in &results {
        let mut line = format!("- {}: {}", result.id, result.state.as_str());
        if let Some(error) = &result.error {
            line.push_str(&format!(" ({}: {})", error.code, error.message));
        }
        lines.push(line);
    }
    Ok(ToolOutcome::ok(lines.join("\n"), json!({"kind": "waited", "run_id": run.run_id, "status": run.status.as_str(), "results": results})))
}
