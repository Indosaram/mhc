use std::collections::BTreeSet;
use serde_json::{json, Value};
pub use super::{naming::{build_mcp_tool_names, McpToolNameEntry}, pagination::{collect_all_pages, McpListPage, McpPaginationResult}};
#[derive(Debug, PartialEq)]
pub struct SchemaConversionResult { pub schema: Value, pub warnings: Vec<String> }
fn permissive() -> Value { json!({"type":"object","properties":{}}) }
pub fn convert_json_schema_to_type_box(schema: &Value) -> SchemaConversionResult {
    let mut stripped = schema.clone();
    if let Some(map) = stripped.as_object_mut() { map.remove("$schema"); map.remove("additionalProperties"); }
    let mut warnings = Vec::new();
    let resolved = resolve_refs(&stripped, schema, &BTreeSet::new(), &mut warnings);
    if !warnings.is_empty() { return SchemaConversionResult { schema: permissive(), warnings }; }
    if !resolved.is_object() { return SchemaConversionResult { schema: permissive(), warnings: vec!["MCP schema is not an object; using permissive object schema.".into()] }; }
    SchemaConversionResult { schema: resolved, warnings }
}
fn resolve_refs(value: &Value, root: &Value, seen: &BTreeSet<String>, warnings: &mut Vec<String>) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|v| resolve_refs(v,root,seen,warnings)).collect()),
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                let resolved = reference.strip_prefix("#/").and_then(|tail| {
                    let mut current = root;
                    for part in tail.split('/') { current = current.as_object()?.get(&part.replace("~1","/").replace("~0","~"))?; }
                    Some(current)
                });
                if seen.contains(reference) || resolved.is_none() {
                    warnings.push(format!("MCP schema contains unresolvable $ref '{reference}'; using permissive object schema."));
                    return permissive();
                }
                let mut next = seen.clone(); next.insert(reference.into());
                return resolve_refs(resolved.unwrap_or(&Value::Null),root,&next,warnings);
            }
            Value::Object(map.iter().filter(|(k,v)| !(k.as_str() == "type" && v.is_null())).map(|(k,v)| (k.clone(),resolve_refs(v,root,seen,warnings))).collect())
        }
        _ => value.clone(),
    }
}
pub fn prepare_output_schema_retry(schema: &Value) -> SchemaConversionResult {
    let mut schema = if schema.is_object() { schema.clone() } else { permissive() };
    let mut warnings = Vec::new();
    if let Some(map) = schema.as_object_mut() {
        for key in ["$schema","additionalProperties"] { if map.remove(key).is_some() { warnings.push(format!("Stripped top-level {key} from MCP outputSchema retry.")); } }
    }
    SchemaConversionResult { schema, warnings }
}
pub fn map_mcp_tool_result(result: &Value) -> Value {
    let mut content = Vec::new();
    for block in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        let mapped = match block.get("type").and_then(Value::as_str) {
            Some("text") => json!({"type":"text","text": block.get("text").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| stringify(block.get("text")))}),
            Some(kind @ ("image" | "audio")) if block.get("data").is_some_and(Value::is_string) && block.get("mimeType").is_some_and(Value::is_string) => json!({"type":kind,"data":block["data"],"mimeType":block["mimeType"]}),
            Some("resource") if block.get("resource").is_some() => json!({"type":"resource","resource":block["resource"]}),
            Some("resource_link") if block.get("uri").is_some_and(Value::is_string) => {
                let mut value = json!({"type":"resource_link","uri":block["uri"]});
                for key in ["name","description","mimeType"] { if let Some(s) = block.get(key).filter(|v| v.is_string()) { value[key] = s.clone(); } }
                value
            }
            _ => json!({"type":"text","text":block.to_string()}),
        };
        content.push(mapped);
    }
    if let Some(structured) = result.get("structuredContent") { content.push(json!({"type":"text","text":structured.to_string()})); }
    if content.is_empty() { content.push(json!({"type":"text","text":"(empty result)"})); }
    if result.get("isError") == Some(&Value::Bool(true)) {
        let message = content.iter().find(|c| c["type"] == "text").and_then(|c| c["text"].as_str()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("MCP tool returned an error result.");
        json!({"ok":false,"error":{"message":message,"content":content}})
    } else { json!({"ok":true,"content":content}) }
}
fn stringify(value: Option<&Value>) -> String { value.map_or_else(|| "undefined".into(), Value::to_string) }
