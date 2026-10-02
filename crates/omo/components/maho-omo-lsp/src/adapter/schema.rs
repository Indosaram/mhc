use serde_json::{Value,json};
pub fn object(properties:Value) -> Value {let required=properties.as_object().into_iter().flat_map(|m|m.iter()).filter(|(_,v)|v.get("optional").is_none()).map(|(k,_)|k.clone()).collect::<Vec<_>>();json!({"type":"object","properties":properties,"required":required})}
pub fn string(description:&str) -> Value {json!({"type":"string","description":description})}
pub fn number(description:&str) -> Value {json!({"type":"number","description":description})}
pub fn boolean(description:&str) -> Value {json!({"type":"boolean","description":description})}
pub fn optional(mut schema:Value) -> Value {schema["optional"]=json!(true);schema}
pub fn literal(value:&str) -> Value {json!({"type":"string","const":value})}
pub fn union(values:&[&str],description:&str) -> Value {json!({"anyOf":values.iter().map(|v|literal(v)).collect::<Vec<_>>(),"description":description})}
#[cfg(test)]
mod tests {use super::*;#[test] fn optional_not_required() {let s=object(json!({"required":string(""),"optional":optional(number(""))}));assert_eq!(s["required"],json!(["required"]));assert_eq!(s["properties"]["optional"]["optional"],true);}}
