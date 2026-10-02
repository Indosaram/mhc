use maho_ext_imagegen::params::DEFAULT_IMAGE_MODEL;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageGenMode { Native, Client, Unavailable }
pub const NATIVE_IMAGE_GEN_TOOL_TYPE: &str = "image_generation";
pub fn apply_image_generation_tools(payload: &Value, mode: ImageGenMode) -> Value {
    let Some(object) = payload.as_object() else { return payload.clone(); };
    let tools = object.get("tools").and_then(Value::as_array);
    let mut kept: Vec<Value> = tools.into_iter().flatten().filter(|tool| {
        let native = tool.get("type").and_then(Value::as_str).is_some_and(|kind| kind == NATIVE_IMAGE_GEN_TOOL_TYPE || kind.starts_with("image_generation_"));
        let client = tool.get("name").and_then(Value::as_str) == Some("generate_image");
        !(native || mode != ImageGenMode::Client && client)
    }).cloned().collect();
    let removed = tools.map_or(0, Vec::len) != kept.len();
    if mode != ImageGenMode::Native && !removed { return payload.clone(); }
    if mode == ImageGenMode::Native { kept.push(json!({"type":NATIVE_IMAGE_GEN_TOOL_TYPE,"model":DEFAULT_IMAGE_MODEL})); }
    let mut result = object.clone();
    result.insert("tools".into(), Value::Array(kept));
    Value::Object(result)
}
