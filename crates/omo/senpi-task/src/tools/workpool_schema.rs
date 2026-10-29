use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;

const NONEMPTY: &str = r"\S";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkpoolToolAgent {
    Category { category: String, prompt: String, model: Option<String> },
    SubagentType { subagent_type: String, prompt: String, model: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolToolItem { pub key: String, pub input: Value }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolToolYieldError { pub code: String, pub message: String }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkpoolToolYieldResult {
    Data { key: String, data: Value },
    Error { key: String, error: WorkpoolToolYieldError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolWorkerYieldParams { pub op: WorkpoolOperation, pub results: Vec<Value> }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolToolParams {
    pub op: WorkpoolOperation,
    #[serde(default)] pub name: Option<String>,
    #[serde(default)] pub agent: Option<WorkpoolToolAgent>,
    #[serde(default)] pub mode: Option<WorkpoolModeInput>,
    #[serde(default)] pub tools: Option<Vec<String>>,
    #[serde(default)] pub pool_id: Option<String>,
    #[serde(default)] pub items: Option<Vec<WorkpoolToolItem>>,
    #[serde(default)] pub results: Option<Vec<WorkpoolToolYieldResult>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolOperation { Create, Push, Close, Inspect, Cancel, Yield }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolModeInput { Fresh, KeepAlive }

fn nonempty() -> Value { json!({"type":"string", "minLength":1, "pattern":NONEMPTY}) }

pub fn workpool_params_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "properties": {
            "op":{"type":"string","enum":["create","push","close","inspect","cancel","yield"]},
            "name":nonempty(),
            "agent":{"oneOf":[
                {"type":"object","additionalProperties":false,"required":["category","prompt"],"properties":{"category":nonempty(),"prompt":nonempty(),"model":nonempty()}},
                {"type":"object","additionalProperties":false,"required":["subagent_type","prompt"],"properties":{"subagent_type":nonempty(),"prompt":nonempty(),"model":nonempty()}}
            ]},
            "mode":{"type":"string","enum":["fresh","keep_alive"]},
            "tools":{"type":"array","items":nonempty()},
            "pool_id":{"type":"string","pattern":"^wp_[0-9a-f]{32}$"},
            "items":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["key","input"],"properties":{"key":nonempty(),"input":{}}}},
            "results":{"type":"array","items":{}}
        }
    })
}

pub static WORKPOOL_PARAMS: LazyLock<Value> = LazyLock::new(workpool_params_schema);
