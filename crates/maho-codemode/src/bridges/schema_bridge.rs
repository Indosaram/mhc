use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalSchemaToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
}

pub type EvalToolCatalog = std::sync::Arc<dyn Fn() -> Result<Vec<EvalSchemaToolInfo>, String> + Send + Sync>;

#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EvalSchemaResult {
    Tools { tools: Vec<String> },
    Tool {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parameters: Option<Value>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("schema() received invalid arguments: {0}")]
    Arguments(String),
    #[error("schema() found no tool named \"{requested}\".{hint}")]
    UnknownTool { requested: String, hint: String },
}

pub fn run_eval_schema(args: &Value, tools: &[EvalSchemaToolInfo]) -> Result<EvalSchemaResult, SchemaError> {
    let object = args.as_object().ok_or_else(|| SchemaError::Arguments("/ Expected object".into()))?;
    if let Some(key) = object.keys().find(|key| key.as_str() != "name") {
        return Err(SchemaError::Arguments(format!("/{key} Unexpected property")));
    }
    let name = match object.get("name") {
        None => return Ok(EvalSchemaResult::Tools { tools: tools.iter().map(|tool| tool.name.clone()).collect() }),
        Some(Value::String(name)) if !name.is_empty() => name,
        Some(Value::String(_)) => return Err(SchemaError::Arguments("/name Expected string length greater or equal to 1".into())),
        Some(_) => return Err(SchemaError::Arguments("/name Expected string".into())),
    };
    if let Some(tool) = tools.iter().find(|tool| tool.name == *name) {
        return Ok(EvalSchemaResult::Tool { name: tool.name.clone(), description: tool.description.clone(), parameters: tool.parameters.clone() });
    }
    let needle = name.to_lowercase();
    let suggestions = tools.iter().filter(|tool| {
        let candidate = tool.name.to_lowercase();
        candidate.contains(&needle) || needle.contains(&candidate)
    }).take(5).map(|tool| tool.name.as_str()).collect::<Vec<_>>();
    Err(SchemaError::UnknownTool { requested: name.clone(), hint: if suggestions.is_empty() { String::new() } else { format!(" Did you mean: {}?", suggestions.join(", ")) } })
}
