use maho_tools::definition::KernelPrelude;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalSchemaToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_prelude: Option<KernelPrelude>,
}

impl EvalSchemaToolInfo {
    pub fn new(
        name: impl Into<String>,
        description: Option<String>,
        parameters: Option<Value>,
    ) -> Self {
        Self {
            name: name.into(),
            description,
            parameters,
            kernel_prelude: None,
        }
    }
}

pub type EvalToolCatalog =
    std::sync::Arc<dyn Fn() -> Result<Vec<EvalSchemaToolInfo>, String> + Send + Sync>;

#[derive(Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EvalSchemaResult {
    Tools {
        tools: Vec<String>,
    },
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

/// `Type.Object({ name: Type.Optional(Type.String({ minLength: 1 })) }, { additionalProperties: false })`.
static SCHEMA_ARGS_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(
    || serde_json::json!({"type":"object","properties":{"name":{"type":"string","minLength":1}},"additionalProperties":false}),
);

pub fn run_eval_schema(
    args: &Value,
    tools: &[EvalSchemaToolInfo],
) -> Result<EvalSchemaResult, SchemaError> {
    if let Some(summary) = super::value_errors::summarize(&SCHEMA_ARGS_SCHEMA, args) {
        return Err(SchemaError::Arguments(summary));
    }
    let Some(Value::String(name)) = args.get("name") else {
        return Ok(EvalSchemaResult::Tools {
            tools: tools.iter().map(|tool| tool.name.clone()).collect(),
        });
    };
    // Documentation-only entries; they are not tools and the listing above stays the ordinary catalog.
    if let Some(virtual_entry) = super::eval_virtual_schemas::virtual_eval_schema(name) {
        return Ok(virtual_entry);
    }
    if let Some(tool) = tools.iter().find(|tool| tool.name == *name) {
        return Ok(EvalSchemaResult::Tool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        });
    }
    let needle = name.to_lowercase();
    let suggestions = tools
        .iter()
        .filter(|tool| {
            let candidate = tool.name.to_lowercase();
            candidate.contains(&needle) || needle.contains(&candidate)
        })
        .take(5)
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    Err(SchemaError::UnknownTool {
        requested: name.clone(),
        hint: if suggestions.is_empty() {
            String::new()
        } else {
            format!(" Did you mean: {}?", suggestions.join(", "))
        },
    })
}
