pub type PauseReasonCallback = Option<Arc<dyn Fn(&str) -> Option<String> + Send + Sync>>;
use crate::tools::tool_text;
use maho_ext_api::{ToolDefinition, ToolError, ToolResult};
use senpi_task::dag::paused_controls::PausedDagGuideCommand;
use senpi_task::pause::guidance::{Guidance, GuidanceError};
use senpi_task::{
    dag::{
        graph::{DagDefinition, DagNodeInput},
        manager::{DagManager, DagManagerError, DagStartParams},
        scheduler::DagNodeControlError,
        types::{DAG_NODE_QUIET_AFTER_MS, DagNode, DagNodeState, DagNodeTarget, DagRunSnapshot},
    },
    tools::task::validation::{TargetInput, TaskTargetSelection, validate_task_target},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const WORKFLOW_TOOL_NAME: &str = "workflow";

pub const DESCRIPTION: &str = "Dependency-graph execution for child tasks. COMPOSE THE SPEC PROGRAMMATICALLY INSIDE AN eval CELL - the kernel exposes this tool as `tool.workflow` alongside a JS SDK, so build the graph as code: partition the work into logically distinct steps, name one node per step, and wire the ordering as data. A graph is a program; write it as one instead of hand-authoring a call. A node starts only after every node it dependsOn has finished, and unrelated nodes run in parallel up to the resident-child cap. dependsOn is ordering ONLY - no upstream output is substituted into a downstream prompt, so each prompt must stand alone. Each node targets EITHER category OR subagent_type, never both; model is an explicit override valid only alongside subagent_type. start is idempotent per definition key: re-starting the same key with the same graph reuses the run instead of duplicating it. wait detaches by default against a live run: the session is woken as each node completes and when the run settles, and detach=false restores the blocking wait that returns the final result. snapshot carries each settled node's own output plus outputBytes, and each running node's lastActivityAt, so a midpoint peek can audit a child's claim and tell a working child from a silent one. When a run settles badly, do NOT start a new one: retry re-runs the failed nodes in place, amend edits the graph and re-runs only what changed, and send steers or revives one node's child.";

#[derive(Deserialize, Debug, Clone)]
pub struct DagToolInput {
    pub action: String,
    pub definition: Option<DefinitionInput>,
    pub run_id: Option<String>,
    pub reason: Option<String>,
    pub detach: Option<bool>,
    pub node_id: Option<String>,
    pub node_ids: Option<Vec<String>>,
    pub message: Option<String>,
    pub prompt: Option<String>,
    /// `guide` only: the guidance record to edit or remove.
    pub guidance_id: Option<String>,
    /// `guide` only: the text of a new or edited guidance record.
    pub text: Option<String>,
    /// `guide` only: remove the named guidance record instead of editing it.
    pub remove: Option<bool>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct DefinitionInput {
    pub key: String,
    pub name: String,
    pub nodes: Vec<NodeInput>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct NodeInput {
    pub id: String,
    pub prompt: String,
    pub label: Option<String>,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "dependsOn")]
    pub depends_on: Option<Vec<String>>,
    pub task_summary: Option<String>,
    pub description: Option<String>,
    pub load_skills: Option<Vec<String>>,
}

/// How the engine delivered a send: steered into a live turn, revived a resident child, queued for
/// host delivery, uncertain (the host did not acknowledge), guidance (stored for a paused run), or
/// held (in the child's input custody until the run resumes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DagToolSendDelivery {
    Steer,
    Revive,
    Queued,
    Uncertain,
    Guidance,
    Held,
}

impl DagToolSendDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Revive => "revive",
            Self::Queued => "queued",
            Self::Uncertain => "uncertain",
            Self::Guidance => "guidance",
            Self::Held => "held",
        }
    }
}

/// One send outcome. `task_id` is optional: a guided send names no child, because the run is paused
/// and the guidance is stored, not delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagToolSendOutcome {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub delivery: DagToolSendDelivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
}

#[derive(Debug)]
pub enum DagToolExecutionError {
    Tool(ToolError),
    Manager(DagManagerError),
    Control(DagNodeControlError),
    /// A guidance refusal (`frozen`, `not_found`, `duplicate_id`) surfaced as its tool vocabulary.
    Guidance(GuidanceError),
}

impl std::fmt::Display for DagToolExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool(e) => write!(f, "{e}"),
            Self::Manager(e) => write!(f, "{e}"),
            Self::Control(e) => write!(f, "{e}"),
            Self::Guidance(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DagToolExecutionError {}

impl From<ToolError> for DagToolExecutionError {
    fn from(e: ToolError) -> Self {
        Self::Tool(e)
    }
}

impl From<DagManagerError> for DagToolExecutionError {
    fn from(e: DagManagerError) -> Self {
        Self::Manager(e)
    }
}

impl From<DagNodeControlError> for DagToolExecutionError {
    fn from(e: DagNodeControlError) -> Self {
        Self::Control(e)
    }
}

impl From<GuidanceError> for DagToolExecutionError {
    fn from(e: GuidanceError) -> Self {
        Self::Guidance(e)
    }
}

/// The tool code a guidance refusal becomes: a dispatched record is frozen, a missing one is gone.
fn guidance_failure(error: GuidanceError) -> ToolResult {
    match error {
        GuidanceError::Frozen => failure(
            "guidance_frozen",
            "Dispatched guidance cannot be changed.",
            vec![],
            vec![],
            vec![],
            vec![],
        ),
        GuidanceError::NotFound => failure(
            "guidance_not_found",
            "Guidance does not exist.",
            vec![],
            vec![],
            vec![],
            vec![],
        ),
        GuidanceError::DuplicateId => failure(
            "invalid_arguments",
            "Guidance ids must be unique.",
            vec![],
            vec![],
            vec![],
            vec![],
        ),
    }
}

pub type DagWait = Arc<dyn Fn(&str, &str) -> Result<Value, ToolError> + Send + Sync>;
pub type DagCancel = Arc<dyn Fn(&str, Option<&str>) -> Result<(), ToolError> + Send + Sync>;
pub type DagRetry = Arc<
    dyn Fn(&str, Option<&[String]>, Option<&str>) -> Result<DagRunSnapshot, DagToolExecutionError>
        + Send
        + Sync,
>;
pub type DagSend = Arc<
    dyn Fn(&str, &str, &str) -> Result<DagToolSendOutcome, DagToolExecutionError> + Send + Sync,
>;
pub type DagAmend =
    Arc<dyn Fn(&str, DagDefinition) -> Result<DagRunSnapshot, DagToolExecutionError> + Send + Sync>;
/// The runtime's paused-child cancellation (`dag-paused-cancel.ts`).
pub type DagCancelNode = Arc<dyn Fn(&str, &str) -> Result<(), DagToolExecutionError> + Send + Sync>;
/// The runtime's paused-run guidance (`dag-tool-guide.ts` + `dag-runtime-controls.ts`).
pub type DagGuide = Arc<
    dyn Fn(&str, PausedDagGuideCommand) -> Result<Guidance, DagToolExecutionError> + Send + Sync,
>;

pub struct DagToolDeps {
    pub manager: DagManager,
    pub parent_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub root_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub wait: Option<DagWait>,
    pub cancel: Option<DagCancel>,
    pub retry: Option<DagRetry>,
    pub send: Option<DagSend>,
    pub amend: Option<DagAmend>,
    /// Injection point for paused-node cancellation; absent until the runtime wires it.
    pub cancel_node: Option<DagCancelNode>,
    /// Injection point for paused-run guidance; absent until the runtime wires it.
    pub guide: Option<DagGuide>,
    /// Why the run is paused right now, when the runtime can tell. A user-stopped run refuses a
    /// model-driven `resume`; nothing else does.
    pub pause_reason: PauseReasonCallback,
}

pub fn quiet_nodes_notice(nodes: &[DagNode], now: i64) -> String {
    let mut quiet = Vec::new();
    for node in nodes {
        if node.state != DagNodeState::Running {
            continue;
        }
        let Some(last_activity) = &node.last_activity_at else {
            continue;
        };
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(last_activity) {
            let activity_ms = parsed.timestamp_millis();
            let silent_ms = now - activity_ms;
            if silent_ms >= DAG_NODE_QUIET_AFTER_MS as i64 {
                quiet.push(format!("{} ({}m)", node.id, silent_ms / 60_000));
            }
        }
    }
    if quiet.is_empty() {
        String::new()
    } else {
        format!(
            " Quiet children, no transcript activity: {}.",
            quiet.join(", ")
        )
    }
}

pub fn lint_dag_definition_nodes(nodes: &[NodeInput]) -> Vec<String> {
    let mut warnings = Vec::new();
    for node in nodes {
        if !node.prompt.contains("TASK:") {
            warnings.push(format!(
                "node \"{}\": prompt is missing the TASK: marker from the workflow node prompt contract (TASK/DELIVERABLE/SCOPE/VERIFY/STOP WHEN)",
                node.id
            ));
        }
        for marker in ["DELIVERABLE:", "SCOPE:", "VERIFY:"] {
            if !node.prompt.contains(marker) {
                warnings.push(format!(
                    "node \"{}\": prompt is missing the {marker} marker from the workflow node prompt contract (TASK/DELIVERABLE/SCOPE/VERIFY/STOP WHEN)",
                    node.id
                ));
            }
        }
        if !node.prompt.contains("STOP WHEN") {
            warnings.push(format!(
                "node \"{}\": prompt is missing a STOP WHEN condition from the workflow node prompt contract",
                node.id
            ));
        }
    }
    if nodes.len() >= 2
        && !nodes.iter().any(|node| {
            [node.id.as_str(), node.prompt.as_str()].iter().any(|text| {
                text.split(|character: char| !character.is_ascii_alphanumeric())
                    .any(|word| {
                        matches!(
                            word.to_ascii_lowercase().as_str(),
                            "verify"
                                | "verification"
                                | "verifier"
                                | "audit"
                                | "validate"
                                | "validation"
                                | "qa"
                        )
                    })
            })
        })
    {
        warnings.push(format!(
            "run has {} nodes but no verification node; a graph that produces work ends with a verification wave",
            nodes.len()
        ));
    }
    warnings
}

fn failure(
    code: &str,
    message: &str,
    nodes: Vec<Value>,
    errors: Vec<Value>,
    diagnostics: Vec<Value>,
    node_ids: Vec<Value>,
) -> ToolResult {
    tool_text(
        message.into(),
        json!({
            "kind": "error",
            "error": {
                "code": code,
                "message": message,
                "nodes": nodes,
                "errors": errors,
                "diagnostics": diagnostics,
                "node_ids": node_ids,
            }
        }),
    )
}

fn manager_failure(error: DagManagerError) -> ToolResult {
    let code = error.code.as_str();
    let errors = error.errors().iter().map(|error| json!({"code": error.code.as_str(), "message": error.message, "nodeIds": error.node_ids})).collect();
    let diagnostics = error
        .diagnostics()
        .iter()
        .map(|diagnostic| json!(diagnostic))
        .collect();
    let node_ids = error.node_ids().iter().map(|id| json!(id)).collect();
    failure(code, &error.message, vec![], errors, diagnostics, node_ids)
}

fn control_failure(error: DagNodeControlError) -> ToolResult {
    let code = error.code.as_str();
    let node_ids = error.node_ids.iter().map(|id| json!(id)).collect();
    failure(code, &error.message, vec![], vec![], vec![], node_ids)
}

fn validate_nodes(definition_nodes: &[NodeInput]) -> Result<Vec<DagNodeInput>, Box<ToolResult>> {
    let mut errors = Vec::new();
    let mut nodes = Vec::new();
    for node in definition_nodes {
        let selection = validate_task_target(TargetInput {
            category: node.category.as_deref(),
            subagent_type: node.subagent_type.as_deref(),
            model: node.model.as_deref(),
        });
        let target = match selection {
            TaskTargetSelection::Error(error) => {
                errors.push(json!({"node_id": node.id, "code": error.code.as_str(), "message": error.message}));
                continue;
            }
            TaskTargetSelection::Category(category) => DagNodeTarget::Category(category),
            TaskTargetSelection::SubagentType(subagent_type) => DagNodeTarget::SubagentType {
                subagent_type,
                model: node.model.clone(),
            },
        };
        nodes.push(DagNodeInput {
            id: node.id.clone(),
            prompt: node.prompt.clone(),
            target,
            label: node.label.clone(),
            depends_on: node.depends_on.clone(),
            task_summary: node.task_summary.clone(),
            description: node.description.clone(),
            load_skills: node.load_skills.clone(),
        });
    }
    if !errors.is_empty() {
        let message = errors
            .iter()
            .map(|error| {
                format!(
                    "Node \"{}\": {}",
                    error["node_id"].as_str().unwrap_or_default(),
                    error["message"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        return Err(Box::new(failure(
            "invalid_definition",
            &message,
            errors,
            vec![],
            vec![],
            vec![],
        )));
    }
    Ok(nodes)
}

fn guidance_state_str(state: senpi_task::pause::guidance::GuidanceState) -> &'static str {
    match state {
        senpi_task::pause::guidance::GuidanceState::Draft => "draft",
        senpi_task::pause::guidance::GuidanceState::Dispatching => "dispatching",
        senpi_task::pause::guidance::GuidanceState::Delivered => "delivered",
        senpi_task::pause::guidance::GuidanceState::Removed => "removed",
    }
}

/// The guidance command the arguments name, or the tool result that refuses them.
fn guide_command(
    input: &DagToolInput,
    snapshot: &DagRunSnapshot,
) -> Result<PausedDagGuideCommand, Box<ToolResult>> {
    if let Some(guidance_id) = &input.guidance_id {
        if input.remove == Some(true) {
            return Ok(PausedDagGuideCommand::Remove {
                id: guidance_id.clone(),
            });
        }
        return match input.text.as_deref().filter(|text| !text.trim().is_empty()) {
            Some(text) => Ok(PausedDagGuideCommand::Edit {
                id: guidance_id.clone(),
                text: text.to_string(),
            }),
            None => Err(Box::new(failure(
                "invalid_arguments",
                "Editing guidance requires text or remove=true.",
                vec![],
                vec![],
                vec![],
                vec![],
            ))),
        };
    }
    let Some(text) = input.text.as_deref().filter(|text| !text.trim().is_empty()) else {
        return Err(Box::new(failure(
            "invalid_arguments",
            "New guidance requires text.",
            vec![],
            vec![],
            vec![],
            vec![],
        )));
    };
    if input.remove == Some(true) {
        return Err(Box::new(failure(
            "invalid_arguments",
            "New guidance requires text.",
            vec![],
            vec![],
            vec![],
            vec![],
        )));
    }
    let node_id = match input.node_id.as_deref() {
        None => None,
        Some(node_id) => match snapshot.nodes.iter().find(|node| node.id == node_id) {
            Some(node) => Some(node.id.clone()),
            None => {
                return Err(Box::new(failure(
                    "node_not_found",
                    "Guidance target does not exist.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                )));
            }
        },
    };
    Ok(PausedDagGuideCommand::Add {
        text: text.to_string(),
        node_id,
    })
}

pub fn run_dag_tool(deps: &DagToolDeps, input: DagToolInput) -> Result<ToolResult, ToolError> {
    if input.action == "start" {
        let Some(definition) = input.definition else {
            return Ok(failure(
                "invalid_definition",
                "action=start requires a definition. Provide definition.key, definition.name, and definition.nodes.",
                vec![],
                vec![],
                vec![],
                vec![],
            ));
        };
        let nodes = match validate_nodes(&definition.nodes) {
            Ok(nodes) => nodes,
            Err(res) => return Ok(*res),
        };
        let warnings = lint_dag_definition_nodes(&definition.nodes);
        let result = match deps.manager.start(DagStartParams {
            definition: DagDefinition {
                key: definition.key,
                name: definition.name,
                nodes,
            },
            parent_session_id: (deps.parent_session_id)(),
            root_session_id: (deps.root_session_id)(),
        }) {
            Ok(result) => result,
            Err(error) => return Ok(manager_failure(error)),
        };
        let verb = if result.reused { "Reused" } else { "Started" };
        let warning_text = if warnings.is_empty() {
            String::new()
        } else {
            format!(
                " Advisory warnings (fix the definition and re-start under a new key to clear them): {}",
                warnings.join(" | ")
            )
        };
        return Ok(tool_text(
            format!(
                "{} dag run {} ({} nodes).{}",
                verb, result.snapshot.run_id, result.snapshot.counts.total, warning_text
            ),
            json!({
                "kind": "started",
                "run_id": result.snapshot.run_id,
                "reused": result.reused,
                "snapshot": result.snapshot,
                "warnings": warnings,
            }),
        ));
    }

    let parent = (deps.parent_session_id)();
    let Some(run_id) = input
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(failure(
            "run_not_found",
            &format!("action={} requires run_id.", input.action),
            vec![],
            vec![],
            vec![],
            vec![],
        ));
    };

    let snapshot = match deps.manager.snapshot(&run_id.to_owned(), &parent) {
        Ok(snapshot) => snapshot,
        Err(error) => return Ok(manager_failure(error)),
    };
    let status = snapshot.status.as_str();

    match input.action.as_str() {
        "attach" => {
            let handle = match deps.manager.attach(&run_id.to_owned(), &parent) {
                Ok(h) => h,
                Err(error) => return Ok(manager_failure(error)),
            };
            let snapshot = handle
                .snapshot()
                .map_err(|error| ToolError::Message(error.to_string()))?;
            let status = snapshot.status.as_str();
            Ok(tool_text(
                format!("Attached to dag run {run_id} ({status})."),
                json!({
                    "kind": "attached",
                    "run_id": run_id,
                    "snapshot": snapshot,
                }),
            ))
        }
        "snapshot" => {
            let quiet = quiet_nodes_notice(&snapshot.nodes, chrono::Utc::now().timestamp_millis());
            Ok(tool_text(
                format!(
                    "Dag run {run_id} is {status} ({}/{} nodes complete).{}",
                    snapshot.counts.completed, snapshot.counts.total, quiet
                ),
                json!({
                    "kind": "snapshot",
                    "run_id": run_id,
                    "snapshot": snapshot,
                }),
            ))
        }
        "wait" => {
            let Some(wait) = &deps.wait else {
                return Ok(tool_text(
                    format!(
                        "Dag run {run_id} is {status}; no wait surface is wired in this session."
                    ),
                    json!({
                        "kind": "waited",
                        "run_id": run_id,
                        "result": {
                            "runId": run_id,
                            "status": status,
                            "snapshot": snapshot,
                            "nodes": {},
                        }
                    }),
                ));
            };
            let is_terminal = matches!(status, "completed" | "failed" | "cancelled");
            let detach = input.detach.unwrap_or(true);
            if detach && !is_terminal {
                let quiet =
                    quiet_nodes_notice(&snapshot.nodes, chrono::Utc::now().timestamp_millis());
                return Ok(tool_text(
                    format!(
                        "Detached from dag run {run_id} ({status}, {}/{} nodes complete). The session is woken as each node completes and again when the run settles; use action=snapshot for a midpoint peek or action=wait with detach=false to block until settle.{}",
                        snapshot.counts.completed, snapshot.counts.total, quiet
                    ),
                    json!({
                        "kind": "detached",
                        "run_id": run_id,
                        "snapshot": snapshot,
                    }),
                ));
            }
            let result = wait(run_id, &parent)?;
            let res_status = result["status"].as_str().unwrap_or_default();
            Ok(tool_text(
                format!("Dag run {run_id} finished with status {res_status}."),
                json!({
                    "kind": "waited",
                    "run_id": run_id,
                    "result": result,
                }),
            ))
        }
        "cancel" => {
            if let Some(cancel) = &deps.cancel {
                cancel(run_id, input.reason.as_deref())?;
            }
            let updated_snapshot = match deps.manager.snapshot(&run_id.to_owned(), &parent) {
                Ok(snapshot) => snapshot,
                Err(error) => return Ok(manager_failure(error)),
            };
            let reason_text = input
                .reason
                .map_or_else(String::new, |reason| format!(" ({reason})"));
            Ok(tool_text(
                format!("Cancelled dag run {run_id}{reason_text}."),
                json!({
                    "kind": "cancelled",
                    "run_id": run_id,
                    "snapshot": updated_snapshot,
                }),
            ))
        }
        "retry" => {
            if input.node_id.is_some() && input.node_ids.is_some() {
                return Ok(failure(
                    "invalid_arguments",
                    "action=retry accepts node_id OR node_ids, never both.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            }
            let selected: Option<Vec<String>> = if let Some(node_id) = input.node_id {
                Some(vec![node_id])
            } else {
                input.node_ids
            };
            if input.prompt.is_some() && selected.as_ref().is_none_or(|ids| ids.len() != 1) {
                return Ok(failure(
                    "invalid_arguments",
                    "action=retry accepts prompt only alongside exactly one node id.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            }
            let Some(retry) = &deps.retry else {
                return Ok(failure(
                    "run_still_active",
                    &format!("Dag run {run_id} has no retry surface wired in this session."),
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let retried_snapshot = match retry(run_id, selected.as_deref(), input.prompt.as_deref())
            {
                Ok(snap) => snap,
                Err(DagToolExecutionError::Manager(e)) => return Ok(manager_failure(e)),
                Err(DagToolExecutionError::Control(e)) => return Ok(control_failure(e)),
                Err(DagToolExecutionError::Guidance(e)) => return Ok(guidance_failure(e)),
                Err(DagToolExecutionError::Tool(e)) => return Err(e),
            };
            let scope = selected.as_ref().map_or_else(
                || "every failed or cancelled node".to_string(),
                |ids| ids.join(", "),
            );
            let mut details = json!({
                "kind": "retried",
                "run_id": run_id,
                "snapshot": retried_snapshot,
            });
            if let Some(ids) = selected {
                details["node_ids"] = json!(ids);
            }
            Ok(tool_text(
                format!(
                    "Retried {scope} on dag run {run_id}; it is now {}.",
                    retried_snapshot.status.as_str()
                ),
                details,
            ))
        }
        "send" => {
            let Some(node_id) = input
                .node_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                return Ok(failure(
                    "invalid_arguments",
                    "action=send requires node_id.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let Some(message) = input
                .message
                .as_deref()
                .filter(|msg| !msg.trim().is_empty())
            else {
                return Ok(failure(
                    "invalid_arguments",
                    "action=send requires a non-empty message.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let Some(send) = &deps.send else {
                return Ok(failure(
                    "node_not_continuable",
                    &format!("Dag run {run_id} has no send surface wired in this session."),
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let sent = match send(run_id, node_id, message) {
                Ok(outcome) => outcome,
                Err(DagToolExecutionError::Manager(e)) => return Ok(manager_failure(e)),
                Err(DagToolExecutionError::Control(e)) => return Ok(control_failure(e)),
                Err(DagToolExecutionError::Guidance(e)) => return Ok(guidance_failure(e)),
                Err(DagToolExecutionError::Tool(e)) => return Err(e),
            };
            let delivery_str = sent.delivery.as_str();
            let text = match sent.delivery {
                DagToolSendDelivery::Guidance => format!(
                    "Stored guidance for dag node {}; the run remains paused.",
                    sent.node_id
                ),
                DagToolSendDelivery::Held => format!(
                    "Held the message in dag node {}; it is delivered when the run resumes.",
                    sent.node_id
                ),
                DagToolSendDelivery::Uncertain => format!(
                    "uncertain: the host did not acknowledge the message to dag node {}; it may still be delivered. omo will confirm by itself; do not resend the same text. Effect id: {}",
                    sent.node_id,
                    sent.effect_id.clone().unwrap_or_default()
                ),
                // Queued is ACCEPTANCE, not delivery: the host still owns the message.
                DagToolSendDelivery::Queued => format!(
                    "Queued the message for dag node {}; pending host delivery.",
                    sent.node_id
                ),
                DagToolSendDelivery::Steer | DagToolSendDelivery::Revive => format!(
                    "Delivered the message to dag node {} ({}).",
                    sent.node_id, delivery_str
                ),
            };
            let mut details = json!({
                "kind": "sent",
                "run_id": run_id,
                "node_id": sent.node_id,
                "delivery": delivery_str,
            });
            if let Some(task_id) = sent.task_id {
                details["task_id"] = json!(task_id);
            }
            if let Some(guidance_id) = sent.guidance_id {
                details["guidance_id"] = json!(guidance_id);
            }
            if let Some(effect_id) = sent.effect_id {
                details["effect_id"] = json!(effect_id);
            }
            if let Some(pos) = sent.queue_position {
                details["queue_position"] = json!(pos);
            }
            Ok(tool_text(text, details))
        }
        "amend" => {
            let Some(definition) = input.definition else {
                return Ok(failure(
                    "invalid_arguments",
                    "action=amend requires a definition carrying the edited graph.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let nodes = match validate_nodes(&definition.nodes) {
                Ok(nodes) => nodes,
                Err(res) => return Ok(*res),
            };
            let Some(amend) = &deps.amend else {
                return Ok(failure(
                    "invalid_amendment",
                    &format!("Dag run {run_id} has no amend surface wired in this session."),
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let amended_snapshot = match amend(
                run_id,
                DagDefinition {
                    key: definition.key,
                    name: definition.name,
                    nodes,
                },
            ) {
                Ok(snap) => snap,
                Err(DagToolExecutionError::Manager(e)) => return Ok(manager_failure(e)),
                Err(DagToolExecutionError::Control(e)) => return Ok(control_failure(e)),
                Err(DagToolExecutionError::Guidance(e)) => return Ok(guidance_failure(e)),
                Err(DagToolExecutionError::Tool(e)) => return Err(e),
            };
            Ok(tool_text(
                format!(
                    "Amended dag run {run_id}; it is now {}.",
                    amended_snapshot.status.as_str()
                ),
                json!({
                    "kind": "amended",
                    "run_id": run_id,
                    "snapshot": amended_snapshot,
                }),
            ))
        }
        "cancel_node" => {
            let Some(node_id) = input
                .node_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                return Ok(failure(
                    "invalid_arguments",
                    "cancel_node requires node_id.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let Some(cancel_node) = &deps.cancel_node else {
                return Ok(failure(
                    "node_not_continuable",
                    "Paused child cancellation is not available in this session.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            match cancel_node(run_id, node_id) {
                Ok(()) => {}
                Err(DagToolExecutionError::Manager(e)) => return Ok(manager_failure(e)),
                Err(DagToolExecutionError::Control(e)) => return Ok(control_failure(e)),
                Err(DagToolExecutionError::Guidance(e)) => return Ok(guidance_failure(e)),
                Err(DagToolExecutionError::Tool(e)) => return Err(e),
            }
            let updated = match deps.manager.snapshot(&run_id.to_owned(), &parent) {
                Ok(snapshot) => snapshot,
                Err(error) => return Ok(manager_failure(error)),
            };
            Ok(tool_text(
                format!("Cancelled dag node {node_id}."),
                json!({"kind":"node_cancelled","run_id":run_id,"node_id":node_id,"snapshot":updated}),
            ))
        }
        "guide" => {
            let Some(guide) = &deps.guide else {
                return Ok(failure(
                    "node_not_continuable",
                    "Guidance is not available in this session.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                ));
            };
            let command = match guide_command(&input, &snapshot) {
                Ok(command) => command,
                Err(result) => return Ok(*result),
            };
            match guide(run_id, command) {
                Ok(guidance) => Ok(tool_text(
                    format!(
                        "Stored guidance {} ({}).",
                        guidance.id,
                        guidance_state_str(guidance.state)
                    ),
                    json!({"kind":"guided","run_id":run_id,"guidance":guidance}),
                )),
                Err(DagToolExecutionError::Guidance(error)) => Ok(guidance_failure(error)),
                Err(DagToolExecutionError::Control(error)) => Ok(control_failure(error)),
                Err(DagToolExecutionError::Manager(error)) => Ok(manager_failure(error)),
                Err(DagToolExecutionError::Tool(error)) => Err(error),
            }
        }
        // Only the user's Resume on the parent session may restart a stopped run; a model calling
        // `resume` is refused with the exact code that says so.
        "resume" => {
            let paused_by_user = deps
                .pause_reason
                .as_ref()
                .and_then(|reason| reason(run_id))
                .as_deref()
                == Some("user_stop");
            Ok(if paused_by_user {
                failure(
                    "run_paused_by_user",
                    "Only the user's Resume on the parent session may restart this run.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                )
            } else {
                failure(
                    "invalid_arguments",
                    "Use the parent session Resume control.",
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                )
            })
        }
        _ => Ok(failure(
            "invalid_arguments",
            &format!("unknown action={}.", input.action),
            vec![],
            vec![],
            vec![],
            vec![],
        )),
    }
}

pub fn create_dag_tool(deps: DagToolDeps) -> ToolDefinition {
    let deps = Arc::new(deps);
    let raw_schema: Value =
        serde_json::from_str(include_str!("../tests/golden/workflow-schema.json"))
            .expect("valid workflow-schema.json");
    let parameters = raw_schema["parameters"].clone();
    let mut tool = ToolDefinition::new(
        WORKFLOW_TOOL_NAME,
        DESCRIPTION,
        parameters,
        Arc::new(move |call| {
            let deps = deps.clone();
            Box::pin(async move {
                let input: DagToolInput = serde_json::from_value(call.params)?;
                // TS awaits these actions; `wait` (and scheduler bookkeeping) block on condvars here, so run
                // them off the async executor. Blocking a current-thread runtime stalls its I/O driver,
                // which the in-process children's provider streams depend on, and deadlocks the run.
                tokio::task::spawn_blocking(move || run_dag_tool(&deps, input))
                    .await
                    .map_err(|error| {
                        ToolError::Message(format!("workflow tool worker failed: {error}"))
                    })?
            })
        }),
    );
    tool.label = "Workflow".into();
    tool
}

pub fn create_workflow_tool(deps: DagToolDeps) -> ToolDefinition {
    create_dag_tool(deps)
}
