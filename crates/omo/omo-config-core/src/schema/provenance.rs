use serde_json::{Map, Value};

use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, array, non_empty_string, optional, required, strict_object,
};

/// Port of the zod `.trim()` on every label: a workspace name, owner and path is stored trimmed, so
/// a padded entry round-trips without the padding and a whitespace-only entry is rejected.
fn trim_workspace_entry(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };
    let mut trimmed: Map<String, Value> = Map::new();
    for (key, entry) in map {
        let next = match (key.as_str(), entry) {
            ("name", Value::String(text)) => Value::String(text.trim().to_string()),
            ("git_owners" | "paths", Value::Array(items)) => Value::Array(
                items
                    .iter()
                    .map(|item| match item {
                        Value::String(text) => Value::String(text.trim().to_string()),
                        other => other.clone(),
                    })
                    .collect(),
            ),
            _ => entry.clone(),
        };
        trimmed.insert(key.clone(), next);
    }
    Value::Object(trimmed)
}

fn omo_provenance_workspace_schema() -> Node {
    Node::Object(Box::new(ObjectSpec {
        fields: vec![
            required("name", non_empty_string()),
            optional("git_owners", array(non_empty_string())),
            optional("paths", array(non_empty_string())),
        ],
        strict: true,
        preprocess: Some(trim_workspace_entry as PreprocessFn),
        refine: None,
    }))
}

/// Workspace labels and the matching rules that decide which checkout belongs to which memory.
pub fn omo_provenance_settings_schema() -> Node {
    strict_object(vec![optional(
        "workspaces",
        array(omo_provenance_workspace_schema()),
    )])
}
