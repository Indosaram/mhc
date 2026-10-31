//! Virtual `tool_schema("eval:*")` entries (upstream `bridges/eval-virtual-schemas.ts` and
//! `bridges/eval-environment-schemas.ts`): documentation a cell reads on demand. They are not tools,
//! never execute anything and never appear in the ordinary catalog listing. The entries are the
//! upstream objects serialized at senpi 903b8336, in upstream map order.

use super::schema_bridge::EvalSchemaResult;
use serde_json::Value;
use std::sync::LazyLock;

static VIRTUAL_ENTRIES: LazyLock<Vec<Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("eval-virtual-schemas.json"))
        .expect("embedded virtual eval schemas are valid JSON")
});

pub fn virtual_eval_schema(name: &str) -> Option<EvalSchemaResult> {
    VIRTUAL_ENTRIES
        .iter()
        .find(|entry| entry["name"] == name)
        .map(|entry| EvalSchemaResult::Tool {
            name: name.to_owned(),
            description: entry["description"].as_str().map(str::to_owned),
            parameters: entry.get("parameters").cloned(),
        })
}

pub fn virtual_eval_schema_names() -> Vec<&'static str> {
    VIRTUAL_ENTRIES
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect()
}
