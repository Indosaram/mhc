use serde_json::Value;
use super::{render_format::{shorten,uri_to_path},language_mappings::SYMBOL_KIND_MAP};
pub const COLLAPSED_HEAD:usize=3;
pub const EXPANDED_HEAD:usize=20;
pub const PATH_BUDGET:usize=80;
pub fn loc_text(loc:&Value)->Result<String,String> {let link=loc.get("targetUri").is_some();let uri=loc.get(if link {"targetUri"} else {"uri"}).and_then(Value::as_str).ok_or("Location URI missing")?;let pos=loc.get(if link {"targetRange"} else {"range"}).and_then(|v|v.get("start")).ok_or("Location range missing")?;let line=pos.get("line").and_then(Value::as_u64).ok_or("Location line missing")?;let character=pos.get("character").and_then(Value::as_u64).ok_or("Location character missing")?;Ok(format!("{}:{}:{character}",shorten(&uri_to_path(uri)?,PATH_BUDGET),line+1))}
pub fn unique<T>(items:Vec<T>,key:impl Fn(&T)->String)->Vec<T> {let mut seen=std::collections::HashSet::new();items.into_iter().filter(|item|seen.insert(key(item))).collect()}
pub fn symbol_kind_name(kind:u32)->String {SYMBOL_KIND_MAP.iter().find(|(k,_)|*k==kind).map_or_else(||format!("Kind({kind})"),|(_,name)|(*name).into())}
#[cfg(test)]
mod tests {use super::*;#[test] fn location_line_one_based() {assert_eq!(loc_text(&serde_json::json!({"uri":"file:///tmp/a.ts","range":{"start":{"line":0,"character":3}}})).unwrap(),"/tmp/a.ts:1:3");}#[test] fn dedupe_preserves_order() {assert_eq!(unique(vec!["b","a","b"],|s|(*s).into()),vec!["b","a"]);}#[test] fn kinds() {assert_eq!(symbol_kind_name(12),"Function");assert_eq!(symbol_kind_name(100),"Kind(100)");}}
