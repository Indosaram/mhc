use std::{collections::HashMap, sync::{Arc, Mutex}};
use maho_ext_api::{AgentToolResult, ContentBlock, ExecuteToolError, ExecuteToolErrorCode, ExecuteToolOptions};
use serde_json::{Value, json};
use super::{output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo};

pub type AgentStatusEmitter = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Default)]
pub struct AgentBridge { isolation_capabilities: Mutex<HashMap<String, bool>> }

#[derive(Debug, thiserror::Error)]
pub enum AgentBridgeError {
    #[error("agent() received invalid arguments: {0}")]
    Arguments(String),
    #[error("agent() unavailable: no \"{0}\" tool is registered in this session")]
    Unavailable(String),
    #[error("agent() requires successful task details with a valid task_id and nonnegative integer run_epoch")]
    InvalidHandle,
    #[error("agent() isolated changes were not applied{0}")]
    IsolationNotApplied(String),
    #[error(transparent)]
    Tool(#[from] ExecuteToolError),
}

impl AgentBridgeError {
    pub fn code(&self) -> Option<&'static str> {
        match self { Self::InvalidHandle => Some("invalid_task_handle"), Self::IsolationNotApplied(_) => Some("isolation_not_applied"), _ => None }
    }
}

pub struct AgentBridgeOptions<'a> {
    pub call_id: &'a str,
    pub task_tool_name: &'a str,
    pub executor: &'a dyn OutputExecuteTool,
    pub tools: Option<&'a [EvalSchemaToolInfo]>,
    pub execute_options: ExecuteToolOptions,
    pub emit_status: Option<AgentStatusEmitter>,
}

impl AgentBridge {
    pub fn for_executor(executor:&Arc<dyn OutputExecuteTool>) -> Arc<Self> {
        type Entry=(std::sync::Weak<dyn OutputExecuteTool>,Arc<AgentBridge>);
        static BRIDGES:std::sync::OnceLock<Mutex<Vec<Entry>>>=std::sync::OnceLock::new();
        let mut bridges=BRIDGES.get_or_init(||Mutex::new(Vec::new())).lock().expect("executor bridge cache lock");
        bridges.retain(|(owner,_)|owner.strong_count()>0);
        let owner=Arc::downgrade(executor);
        if let Some((_,bridge))=bridges.iter().find(|(existing,_)|std::sync::Weak::ptr_eq(existing,&owner)) {return bridge.clone();}
        let bridge=Arc::new(Self::default());
        bridges.push((owner,bridge.clone()));
        bridge
    }

    pub async fn run(&self, args: &Value, options: AgentBridgeOptions<'_>) -> Result<Value, AgentBridgeError> {
        let object = args.as_object().ok_or_else(|| AgentBridgeError::Arguments("Expected object".into()))?;
        for (key, value) in object {
            let valid = match key.as_str() {
                "prompt" | "agent" | "model" => value.as_str().is_some_and(|text| !text.is_empty()),
                "label" => value.is_string(), "schema" => true,
                "handle" | "isolated" | "apply" => value.is_boolean(),
                "tools" => value.as_array().is_some_and(|values| values.iter().all(|value| value.as_str().is_some_and(|text| !text.is_empty()))),
                "merge" => value.is_boolean() || matches!(value.as_str(), Some("patch" | "branch")),
                _ => false,
            };
            if !valid { return Err(AgentBridgeError::Arguments(format!("/{key} invalid value"))); }
        }
        let prompt = object.get("prompt").and_then(Value::as_str).ok_or_else(|| AgentBridgeError::Arguments("/prompt required".into()))?;
        let structured = object.contains_key("schema");
        let supports_isolation = {
            let mut capabilities = self.isolation_capabilities.lock().expect("agent capability cache poisoned");
            *capabilities.entry(options.task_tool_name.into()).or_insert_with(|| options.tools.and_then(|tools| tools.iter().find(|tool| tool.name == options.task_tool_name)).and_then(|tool| tool.parameters.as_ref()).is_some_and(|schema| schema["properties"].get("isolated").is_some()))
        };
        let warning = (!supports_isolation && ["isolated", "apply", "merge"].iter().any(|key| object.contains_key(*key))).then_some("isolated/apply/merge unsupported (no isolation in task engine)");
        let fallback_id = object.get("label").and_then(Value::as_str).unwrap_or(options.call_id).to_owned();
        if let Some(warning) = warning && let Some(emit) = &options.emit_status { emit(json!({"op":"agent","id":fallback_id,"status":"running","warning":warning})); }
        if options.executor.is_tool_available(options.task_tool_name) == Some(false) { return Err(AgentBridgeError::Unavailable(options.task_tool_name.into())); }
        let prompt = if structured { format!("{prompt}\n\nRespond ONLY with JSON matching this JSON-Schema:\n{}", args["schema"]) } else { prompt.into() };
        let handle = args["handle"] == true;
        let mut params = json!({"prompt":prompt,"run_in_background":handle});
        for (source, target) in [("agent", "subagent_type"), ("model", "model"), ("label", "name"), ("tools", "tools")] { if let Some(value) = object.get(source) { params[target] = value.clone(); } }
        if supports_isolation {
            for key in ["isolated", "apply", "merge"] {
                if let Some(value) = object.get(key) { params[key] = match (key, value.as_bool()) { ("merge", Some(true)) => json!("branch"), ("merge", Some(false)) => json!("patch"), _ => value.clone() }; }
            }
        }
        let mut execute_options = options.execute_options;
        if let Some(emit) = options.emit_status {
            execute_options.on_update = Some(Arc::new(move |update| {
                let first = |keys: &[&str]| keys.iter().find_map(|key| update.details[*key].as_str().filter(|text| !text.is_empty()).map(str::to_owned));
                let mut event = json!({"op":"agent","id":first(&["task_id","taskId","id"]).unwrap_or_else(|| fallback_id.clone()),"status":first(&["status"]).unwrap_or_else(|| "running".into())});
                if let Some(agent) = first(&["subagent_type","agent"]) { event["agent"] = json!(agent); }
                if let Some(warning) = warning { event["warning"] = json!(warning); }
                emit(event);
            }));
        }
        let result = options.executor.execute_tool(options.task_tool_name, params, execute_options).await.map_err(|error| match error.code {
            ExecuteToolErrorCode::UnknownTool | ExecuteToolErrorCode::InactiveTool => AgentBridgeError::Unavailable(options.task_tool_name.into()),
            _ => AgentBridgeError::Tool(error),
        })?;
        format_result(&result, structured, handle)
    }
}

fn format_result(result: &AgentToolResult, structured: bool, handle: bool) -> Result<Value, AgentBridgeError> {
    let text = result.content.iter().filter_map(|part| match part { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
    let mut value = json!({"text":text});
    let isolation = result.details["isolation"].as_object();
    if let Some(isolation) = isolation { value["details"] = json!({"isolation":isolation}); }
    if handle {
        let id = result.details["task_id"].as_str().filter(|id| id.strip_prefix("st_").is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))));
        let epoch = result.details["run_epoch"].as_u64();
        if result.is_error == Some(true) || result.details["isError"] == true || result.details.get("error").is_some() || id.is_none() || epoch.is_none() { return Err(AgentBridgeError::InvalidHandle); }
        if let (Some(id), Some(epoch)) = (id, epoch) { value["id"] = json!(id); value["handle"] = json!(format!("agent://{id}")); value["run_epoch"] = json!(epoch); }
    } else {
        if let Some(isolation) = isolation && isolation.get("changes_applied") == Some(&json!(false)) {
            let recovery = ["patch_path","branch_name","manual_command"].iter().filter_map(|key| isolation.get(*key).and_then(Value::as_str).map(|value| format!("{key}: {value}"))).collect::<Vec<_>>().join("; ");
            return Err(AgentBridgeError::IsolationNotApplied(if recovery.is_empty() { String::new() } else { format!("; {recovery}") }));
        }
        if structured { match serde_json::from_str::<Value>(&text) { Ok(data) => value["data"] = data, Err(error) => value["parseError"] = json!(error.to_string()) } }
    }
    Ok(value)
}
