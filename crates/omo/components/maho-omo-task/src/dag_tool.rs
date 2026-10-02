use std::sync::Arc;
use serde::Deserialize;
use serde_json::{Value, json};
use maho_ext_api::{ToolDefinition, ToolError, ToolResult};
use senpi_task::{dag::{graph::{DagDefinition, DagNodeInput}, manager::{DagManager, DagManagerError, DagManagerErrorCode, DagStartParams}, types::DagNodeTarget}, tools::task::validation::{TargetInput, TaskTargetSelection, validate_task_target}};
use crate::tools::tool_text;

pub const DAG_TOOL_NAME: &str = "dag";
pub const DESCRIPTION: &str = "Run a dependency graph of child tasks in one call: nodes execute in parallel waves, and a node starts only after every node it dependsOn has finished. dependsOn is ordering ONLY - no upstream output is substituted into a downstream prompt, so each prompt must stand alone. Each node targets EITHER category OR subagent_type, never both; model is an explicit override valid only alongside subagent_type. start is idempotent per definition key: re-starting the same key with the same graph reuses the run instead of duplicating it.";
#[derive(Deserialize)]
pub struct DagToolInput { pub action: String, pub definition: Option<DefinitionInput>, pub run_id: Option<String>, pub reason: Option<String> }
#[derive(Deserialize)]
pub struct DefinitionInput { pub key: String, pub name: String, pub nodes: Vec<NodeInput> }
#[derive(Deserialize)]
pub struct NodeInput {
    pub id: String, pub prompt: String, pub label: Option<String>, pub category: Option<String>, pub subagent_type: Option<String>, pub model: Option<String>,
    #[serde(rename = "dependsOn")]
    pub depends_on: Option<Vec<String>>,
    pub task_summary: Option<String>, pub description: Option<String>, pub load_skills: Option<Vec<String>>,
}
pub type DagWait = Arc<dyn Fn(&str, &str) -> Result<Value, ToolError> + Send + Sync>;
pub type DagCancel = Arc<dyn Fn(&str, Option<&str>) -> Result<(), ToolError> + Send + Sync>;
pub struct DagToolDeps { pub manager: DagManager, pub parent_session_id: Arc<dyn Fn() -> String + Send + Sync>, pub root_session_id: Arc<dyn Fn() -> String + Send + Sync>, pub wait: Option<DagWait>, pub cancel: Option<DagCancel> }
fn failure(code: &str, message: &str, nodes: Vec<Value>, errors: Vec<Value>, diagnostics: Vec<Value>) -> ToolResult {
    tool_text(message.into(), json!({"kind":"error","error":{"code":code,"message":message,"nodes":nodes,"errors":errors,"diagnostics":diagnostics}}))
}
fn manager_failure(error: DagManagerError) -> ToolResult {
    let code = match error.code { DagManagerErrorCode::DefinitionConflict => "definition_conflict", DagManagerErrorCode::RunNotFound => "run_not_found", DagManagerErrorCode::RunNotOwned => "run_not_owned", DagManagerErrorCode::InvalidArguments | DagManagerErrorCode::InvalidDefinition => "invalid_definition" };
    let errors = error.errors.iter().map(|error| json!({"code":error.code.as_str(),"message":error.message,"nodeIds":error.node_ids})).collect();
    let diagnostics = error.diagnostics.iter().map(|diagnostic| json!(diagnostic)).collect();
    failure(code, &error.message, vec![], errors, diagnostics)
}
pub fn run_dag_tool(deps: &DagToolDeps, input: DagToolInput) -> Result<ToolResult, ToolError> {
    let parent = (deps.parent_session_id)();
    if input.action == "start" {
        let Some(definition) = input.definition else { return Ok(failure("invalid_definition", "action=start requires a definition. Provide definition.key, definition.name, and definition.nodes.", vec![], vec![], vec![])) };
        let mut errors = Vec::new(); let mut nodes = Vec::new();
        for node in definition.nodes {
            let selection = validate_task_target(TargetInput { category: node.category.as_deref(), subagent_type: node.subagent_type.as_deref(), model: node.model.as_deref() });
            let target = match selection {
                TaskTargetSelection::Error(error) => { errors.push(json!({"node_id":node.id,"code":error.code.as_str(),"message":error.message})); continue },
                TaskTargetSelection::Category(category) => DagNodeTarget::Category(category),
                TaskTargetSelection::SubagentType(subagent_type) => DagNodeTarget::SubagentType { subagent_type, model: node.model },
            };
            nodes.push(DagNodeInput { id: node.id, prompt: node.prompt, target, label: node.label, depends_on: node.depends_on, task_summary: node.task_summary, description: node.description, load_skills: node.load_skills });
        }
        if !errors.is_empty() { let message = errors.iter().map(|error| format!("Node \"{}\": {}", error["node_id"].as_str().unwrap_or_default(), error["message"].as_str().unwrap_or_default())).collect::<Vec<_>>().join(" "); return Ok(failure("invalid_definition", &message, errors, vec![], vec![])) }
        let result = match deps.manager.start(DagStartParams { definition: DagDefinition { key: definition.key, name: definition.name, nodes }, parent_session_id: parent, root_session_id: (deps.root_session_id)() }) { Ok(result) => result, Err(error) => return Ok(manager_failure(error)) };
        return Ok(tool_text(format!("{} dag run {} ({} nodes).", if result.reused { "Reused" } else { "Started" }, result.snapshot.run_id, result.snapshot.counts.total), json!({"kind":"started","run_id":result.snapshot.run_id,"reused":result.reused,"snapshot":result.snapshot})));
    }
    let Some(run_id) = input.run_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) else { return Ok(failure("run_not_found", &format!("action={} requires run_id.", input.action), vec![], vec![], vec![])) };
    let snapshot = match deps.manager.snapshot(&run_id.to_owned(), &parent) { Ok(snapshot) => snapshot, Err(error) => return Ok(manager_failure(error)) };
    let status = snapshot.status.as_str();
    match input.action.as_str() {
        "attach" => Ok(tool_text(format!("Attached to dag run {run_id} ({status})."), json!({"kind":"attached","run_id":run_id,"snapshot":snapshot}))),
        "snapshot" => Ok(tool_text(format!("Dag run {run_id} is {status} ({}/{} nodes complete).", snapshot.counts.completed, snapshot.counts.total), json!({"kind":"snapshot","run_id":run_id,"snapshot":snapshot}))),
        "wait" => if let Some(wait) = &deps.wait { let result = wait(run_id, &parent)?; Ok(tool_text(format!("Dag run {run_id} finished with status {}.", result["status"].as_str().unwrap_or_default()), json!({"kind":"waited","run_id":run_id,"result":result}))) } else { Ok(tool_text(format!("Dag run {run_id} is {status}; no wait surface is wired in this session."), json!({"kind":"waited","run_id":run_id,"result":{"runId":run_id,"status":status,"snapshot":snapshot,"nodes":{}}}))) },
        "cancel" => { if let Some(cancel) = &deps.cancel { cancel(run_id, input.reason.as_deref())?; } let snapshot = match deps.manager.snapshot(&run_id.to_owned(), &parent) { Ok(snapshot) => snapshot, Err(error) => return Ok(manager_failure(error)) }; Ok(tool_text(format!("Cancelled dag run {run_id}{}.", input.reason.map_or_else(String::new, |reason| format!(" ({reason})"))), json!({"kind":"cancelled","run_id":run_id,"snapshot":snapshot}))) },
        _ => Err(ToolError::Message("invalid dag action".into())),
    }
}
pub fn create_dag_tool(deps: DagToolDeps) -> ToolDefinition {
    let deps = Arc::new(deps);
    let parameters = json!({"type":"object","properties":{"action":{"anyOf":[{"type":"string","const":"start"},{"type":"string","const":"attach"},{"type":"string","const":"snapshot"},{"type":"string","const":"wait"},{"type":"string","const":"cancel"}]},"definition":{"type":"object","properties":{"key":{"type":"string"},"name":{"type":"string"},"nodes":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"prompt":{"type":"string"},"label":{"type":"string"},"category":{"type":"string"},"subagent_type":{"type":"string"},"model":{"type":"string"},"dependsOn":{"type":"array","items":{"type":"string"}},"task_summary":{"type":"string"},"description":{"type":"string"},"load_skills":{"type":"array","items":{"type":"string"}}},"required":["id","prompt"]}}},"required":["key","name","nodes"]},"run_id":{"type":"string"},"reason":{"type":"string"}},"required":["action"]});
    let mut tool = ToolDefinition::new(DAG_TOOL_NAME, DESCRIPTION, parameters, Arc::new(move |call| { let deps = deps.clone(); Box::pin(async move { run_dag_tool(&deps, serde_json::from_value(call.params)?) }) }));
    tool.parameters["properties"]["action"]["description"]=json!("start creates or reuses a run from a definition; attach re-binds to a live run; snapshot reads current state; wait blocks until the run settles; cancel stops it.");
    tool.parameters["properties"]["definition"]["description"]=json!("Graph to run. Required for action=start, ignored otherwise.");
    tool.parameters["properties"]["run_id"]["description"]=json!("Run id returned by start. Required for attach, snapshot, wait, and cancel.");
    tool.parameters["properties"]["reason"]["description"]=json!("Optional human-readable reason recorded when cancelling a run.");
    let definition=&mut tool.parameters["properties"]["definition"]["properties"];
    definition["key"]["description"]=json!("Stable idempotency key for this run within the session; re-starting with the same key and definition reuses the existing run.");
    definition["name"]["description"]=json!("Human-readable run name shown in status views.");
    definition["nodes"]["description"]=json!("The nodes of the graph. Each node targets EITHER a category OR a subagent_type.");
    let node=&mut definition["nodes"]["items"]["properties"];
    for (key,description) in [
        ("id","Node id, unique within the definition; referenced by dependsOn."),
        ("prompt","The instruction for this node's child task. MUST be written in English."),
        ("label","Short human label for this node."),
        ("category","Category name to route this node through. Mutually exclusive with subagent_type; required unless subagent_type is given."),
        ("subagent_type","Agent name to invoke directly (e.g. momus). Mutually exclusive with category; required unless category is given."),
        ("model","Explicit model override. Only valid with subagent_type; rejected alongside category, which takes its model from omo.json."),
        ("dependsOn","Ids of nodes that must finish before this one is scheduled. Ordering only: no output is substituted into this prompt."),
        ("task_summary","One-line summary of this node's work, shown in the run widget."),
        ("description","Short human description of this node."),
        ("load_skills","Skill names whose SKILL.md content is prepended to this node's prompt."),
    ] { node[key]["description"]=json!(description); }
    tool.label = "Dag".into(); tool
}
