//! The `workpool` tool (spec §1, §2): strict parameter validation, scope rules, dispatch to the
//! engine, and the JSON response shapes. Failures are tool errors whose text starts with the
//! machine code.
use crate::engine::{CreateRequest, PoolEngine};
use crate::model::{AgentSpec, PoolError, PoolMode, codes};
use serde_json::{Map, Value, json};

/// Tool name (interface fact).
pub const TOOL_NAME: &str = "workpool";

pub const DESCRIPTION: &str = "Keyed work pool: run many similar items through one worker agent and get all keyed results back in one message.\n\
op \"create\" {name, agent: {category|subagent_type, prompt, model?}, mode?: fresh|keep_alive, tools?} returns {pool_id}; \
\"push\" {pool_id, items: [{key, input}]} queues items (same key + same input is a no-op); \
\"close\" {pool_id} means no more items: once every item finishes, one `workpool-aggregate` message with the keyed results is delivered and starts a turn (do not poll); \
\"inspect\" {pool_id} shows items and workers; \"cancel\" {pool_id} stops everything. \
Workers report each item with op \"yield\" {results: [{key, data} | {key, error: {code, message}}]}.";

/// Description of the yield-only view workers get.
pub const WORKER_DESCRIPTION: &str = "Report the results of your work pool assignment. \
op \"yield\" {results: [{key, data} | {key, error: {code, message}}]}: one entry per assigned key, each key once.";

/// Who is calling (spec §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A session with a host identity that is not a worker.
    Parent { session: String },
    /// A worker task of `pool_id`.
    Worker { pool_id: String, task_id: String },
    /// No host identity.
    Anonymous,
}

/// The tool's answer: the JSON response (also its text) or an error.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub text: String,
    pub details: Value,
    pub is_error: bool,
}

impl ToolOutcome {
    fn ok(value: Value) -> Self {
        Self { text: value.to_string(), details: value, is_error: false }
    }
    fn err(error: &PoolError) -> Self {
        Self {
            text: error.to_string(),
            details: json!({"code": error.code, "message": error.message}),
            is_error: true,
        }
    }
    /// The error code of a failed outcome.
    pub fn code(&self) -> Option<&str> {
        if self.is_error { self.details.get("code").and_then(Value::as_str) } else { None }
    }
}

fn invalid(message: impl Into<String>) -> PoolError {
    PoolError::new(codes::INVALID_INPUT, message)
}

/// JSON schema of the parent tool.
pub fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "op": {"type": "string", "enum": ["create", "push", "close", "inspect", "cancel", "yield"]},
            "name": {"type": "string", "description": "create: pool name (idempotent per session)"},
            "agent": {"type": "object", "description": "create: {category, prompt, model?} or {subagent_type, prompt, model?}",
                "properties": {"category": {"type": "string"}, "subagent_type": {"type": "string"}, "prompt": {"type": "string"}, "model": {"type": "string"}}},
            "mode": {"type": "string", "enum": ["fresh", "keep_alive"], "description": "create: default keep_alive"},
            "tools": {"type": "array", "items": {"type": "string"}, "description": "create: extra tools of this session that workers may call"},
            "pool_id": {"type": "string"},
            "items": {"type": "array", "items": {"type": "object", "properties": {"key": {"type": "string"}, "input": {}}, "required": ["key", "input"]}},
            "results": {"type": "array", "items": {"type": "object"}, "description": "yield (workers only)"}
        },
        "required": ["op"]
    })
}

/// JSON schema of the workers' yield-only view.
pub fn worker_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "op": {"type": "string", "enum": ["yield"]},
            "results": {"type": "array", "items": {"type": "object", "properties": {
                "key": {"type": "string"}, "data": {},
                "error": {"type": "object", "properties": {"code": {"type": "string"}, "message": {"type": "string"}}}
            }}}
        },
        "required": ["op", "results"]
    })
}

fn object(params: &Value) -> Result<&Map<String, Value>, PoolError> {
    params.as_object().ok_or_else(|| invalid("parameters must be an object"))
}

/// Checks that `params` has exactly `required` plus any of `optional`.
fn fields(params: &Map<String, Value>, op: &str, required: &[&str], optional: &[&str]) -> Result<(), PoolError> {
    for key in params.keys() {
        if key != "op" && !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(invalid(format!("op {op} does not accept field {key}")));
        }
    }
    for key in required {
        if !params.contains_key(*key) {
            return Err(invalid(format!("op {op} requires field {key}")));
        }
    }
    Ok(())
}

fn string(params: &Map<String, Value>, key: &str) -> Result<String, PoolError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| invalid(format!("{key} must be a string")))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid(format!("{key} must not be blank")));
    }
    Ok(trimmed.to_owned())
}

/// Tool-boundary pool id check: malformed ids are `invalid_input` (spec §1).
fn pool_id(params: &Map<String, Value>) -> Result<String, PoolError> {
    let id = string(params, "pool_id")?;
    crate::model::validate_pool_id(&id).map_err(|error| invalid(error.message))?;
    Ok(id)
}

/// Parses `agent` (spec §1).
///
/// # Errors
/// `invalid_input`.
pub fn parse_agent(value: &Value) -> Result<AgentSpec, PoolError> {
    let map = value.as_object().ok_or_else(|| invalid("agent must be an object"))?;
    let has_category = map.contains_key("category");
    let has_subagent = map.contains_key("subagent_type");
    if has_category == has_subagent {
        return Err(invalid("agent needs exactly one of category or subagent_type"));
    }
    let kind = if has_category { "category" } else { "subagent_type" };
    for key in map.keys() {
        if key != kind && key != "prompt" && key != "model" {
            return Err(invalid(format!("agent does not accept field {key}")));
        }
    }
    let target = string(map, kind).map_err(|e| invalid(format!("agent.{}", e.message)))?;
    let prompt = string(map, "prompt").map_err(|e| invalid(format!("agent.{}", e.message)))?;
    let model = if map.contains_key("model") {
        Some(string(map, "model").map_err(|e| invalid(format!("agent.{}", e.message)))?)
    } else {
        None
    };
    Ok(if has_category {
        AgentSpec::Category { category: target, prompt, model }
    } else {
        AgentSpec::Subagent { subagent_type: target, prompt, model }
    })
}

/// Parses the `create` fields.
///
/// # Errors
/// `invalid_input`.
pub fn parse_create(params: &Map<String, Value>) -> Result<CreateRequest, PoolError> {
    fields(params, "create", &["name", "agent"], &["mode", "tools"])?;
    let name = string(params, "name")?;
    let agent = parse_agent(&params["agent"])?;
    let mode = match params.get("mode") {
        None => PoolMode::KeepAlive,
        Some(_) => match string(params, "mode")?.as_str() {
            "fresh" => PoolMode::Fresh,
            "keep_alive" => PoolMode::KeepAlive,
            other => return Err(invalid(format!("mode must be fresh or keep_alive, not {other}"))),
        },
    };
    let tools = match params.get("tools") {
        None => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|tool| match tool.as_str().map(str::trim) {
                Some(name) if !name.is_empty() => Ok(name.to_owned()),
                _ => Err(invalid("tools must be non-blank strings")),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("tools must be a list of tool names")),
    };
    Ok(CreateRequest { name, agent, mode, tools })
}

/// Parses `push.items`.
///
/// # Errors
/// `invalid_input`.
pub fn parse_items(value: &Value) -> Result<Vec<(String, Value)>, PoolError> {
    let list = value.as_array().ok_or_else(|| invalid("items must be a list"))?;
    list.iter()
        .map(|item| {
            let map = item.as_object().ok_or_else(|| invalid("each item must be an object {key, input}"))?;
            if map.len() != 2 || !map.contains_key("key") || !map.contains_key("input") {
                return Err(invalid("each item has exactly the fields key and input"));
            }
            Ok((string(map, "key")?, map["input"].clone()))
        })
        .collect()
}

fn op_of(params: &Map<String, Value>) -> Result<String, PoolError> {
    string(params, "op")
}

/// Runs one tool call.
pub fn run(engine: &PoolEngine, caller: &Caller, params: &Value) -> ToolOutcome {
    match dispatch(engine, caller, params) {
        Ok(value) => ToolOutcome::ok(value),
        Err(error) => ToolOutcome::err(&error),
    }
}

fn dispatch(engine: &PoolEngine, caller: &Caller, params: &Value) -> Result<Value, PoolError> {
    let params = object(params)?;
    let op = op_of(params)?;
    if !matches!(op.as_str(), "create" | "push" | "close" | "inspect" | "cancel" | "yield") {
        return Err(invalid(format!("unknown op {op}")));
    }
    let scope = |message: &str| PoolError::new(codes::SCOPE_DENIED, message);
    if op == "yield" {
        fields(params, "yield", &["results"], &[])?;
        let results = params["results"].as_array().ok_or_else(|| invalid("results must be a list"))?;
        return match caller {
            Caller::Worker { pool_id, task_id } => {
                let receipts = engine.yield_results(pool_id, task_id, results)?;
                Ok(json!({"receipts": receipts}))
            }
            Caller::Parent { .. } => Err(scope("only a pool's assigned worker may yield")),
            Caller::Anonymous => Err(scope("this session has no host identity")),
        };
    }
    let session = match caller {
        Caller::Parent { session } => session.as_str(),
        Caller::Worker { .. } => return Err(scope("a pool worker may not manage pools")),
        Caller::Anonymous => return Err(scope("this session has no host identity")),
    };
    match op.as_str() {
        "create" => {
            let request = parse_create(params)?;
            let record = engine.create(session, &request)?;
            Ok(json!({
                "pool_id": record.pool_id,
                "mode": record.mode.as_str(),
                "generation": record.generation,
                "status": record.status.as_str(),
            }))
        }
        "push" => {
            fields(params, "push", &["pool_id", "items"], &[])?;
            let id = pool_id(params)?;
            let items = parse_items(&params["items"])?;
            let ids = engine.push(session, &id, items)?;
            let ids: Vec<Value> = ids.into_iter().map(|(key, item_id)| json!({"key": key, "item_id": item_id})).collect();
            Ok(json!({"pool_id": id, "item_ids": ids}))
        }
        "close" | "cancel" => {
            fields(params, &op, &["pool_id"], &[])?;
            let id = pool_id(params)?;
            let record = if op == "close" { engine.close(session, &id)? } else { engine.cancel(session, &id)? };
            Ok(json!({"pool_id": id, "status": record.status.as_str(), "generation": record.generation}))
        }
        _ => {
            fields(params, "inspect", &["pool_id"], &[])?;
            let id = pool_id(params)?;
            Ok(engine.inspect(session, &id)?.inspect_json())
        }
    }
}
