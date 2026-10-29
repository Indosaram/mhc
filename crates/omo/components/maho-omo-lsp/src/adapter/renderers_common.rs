use serde_json::Value;
use super::{render_format::{shorten,uri_to_path},language_mappings::SYMBOL_KIND_MAP};
pub const COLLAPSED_HEAD:usize=3;
pub const EXPANDED_HEAD:usize=20;
pub const PATH_BUDGET:usize=80;
pub trait RendererTheme {fn fg(&self,color:&str,text:&str)->String;fn bold(&self,text:&str)->String;}
impl RendererTheme for maho_ext_api::Theme {fn fg(&self,color:&str,text:&str)->String {maho_ext_api::Theme::fg(self,color,text)} fn bold(&self,text:&str)->String {format!("\u{1b}[1m{text}\u{1b}[22m")}}
#[derive(Debug,Clone,Copy,Default,PartialEq,Eq)]
pub struct RenderResultOptions {pub expanded:bool,pub is_partial:bool}
#[derive(Debug,Clone,Copy,Default,PartialEq,Eq)]
pub struct PositionArgs<'a> {pub file_path:&'a str,pub line:u64,pub character:u64}
pub fn loc_text(loc:&Value)->Result<String,String> {let link=loc.get("targetUri").is_some();let uri=loc.get(if link {"targetUri"} else {"uri"}).and_then(Value::as_str).ok_or("Location URI missing")?;let pos=loc.get(if link {"targetRange"} else {"range"}).and_then(|v|v.get("start")).ok_or("Location range missing")?;let line=pos.get("line").and_then(Value::as_u64).ok_or("Location line missing")?;let character=pos.get("character").and_then(Value::as_u64).ok_or("Location character missing")?;Ok(format!("{}:{}:{character}",shorten(&uri_to_path(uri)?,PATH_BUDGET),line+1))}
pub fn unique<T>(items:Vec<T>,key:impl Fn(&T)->String)->Vec<T> {let mut seen=std::collections::HashSet::new();items.into_iter().filter(|item|seen.insert(key(item))).collect()}
pub fn symbol_kind_name(kind:u32)->String {SYMBOL_KIND_MAP.iter().find(|(k,_)|*k==kind).map_or_else(||format!("Kind({kind})"),|(_,name)|(*name).into())}
pub fn result_details(result:&Value)->Option<&Value> {result.get("details").filter(|details|!details.is_null())}
pub fn result_text(result:&Value,index:usize)->Option<&str> {result.get("content").and_then(Value::as_array).and_then(|blocks|blocks.get(index)).and_then(|block|block.get("text")).and_then(Value::as_str)}
pub fn render_position_call(tool_name:&str,args:&PositionArgs<'_>,theme:&dyn RendererTheme)->String {let head=theme.fg("toolTitle",&theme.bold(&format!("{tool_name} ")));let loc=theme.fg("accent",&format!("{}:{}:{}",shorten(args.file_path,PATH_BUDGET),args.line,args.character));head+&loc}
pub fn position_args(args:&Value)->PositionArgs<'_> {PositionArgs {file_path:args.get("filePath").and_then(Value::as_str).unwrap_or(""),line:args.get("line").and_then(Value::as_u64).unwrap_or(0),character:args.get("character").and_then(Value::as_u64).unwrap_or(0)}}
#[cfg(test)]
pub(crate) mod test_theme {use super::RendererTheme;pub struct TagTheme;impl RendererTheme for TagTheme {fn fg(&self,color:&str,text:&str)->String {format!("<{color}>{text}</{color}>")}fn bold(&self,text:&str)->String {format!("<b>{text}</b>")}}}
#[cfg(test)]
mod tests {use super::*;use crate::adapter::renderers_common::test_theme::TagTheme;#[test] fn location_line_one_based() {assert_eq!(loc_text(&serde_json::json!({"uri":"file:///tmp/a.ts","range":{"start":{"line":0,"character":3}}})).unwrap(),"/tmp/a.ts:1:3");}#[test] fn dedupe_preserves_order() {assert_eq!(unique(vec!["b","a","b"],|s|(*s).into()),vec!["b","a"]);}#[test] fn kinds() {assert_eq!(symbol_kind_name(12),"Function");assert_eq!(symbol_kind_name(100),"Kind(100)");}#[test] fn position_call_wraps_tool_title_and_location(){assert_eq!(render_position_call("lsp_symbols",&PositionArgs {file_path:"/tmp/a.ts",line:2,character:3},&TagTheme),"<toolTitle><b>lsp_symbols </b></toolTitle><accent>/tmp/a.ts:2:3</accent>");}#[test] fn position_call_shortens_long_paths(){let long=format!("/{}","x".repeat(PATH_BUDGET));let rendered=render_position_call("t",&PositionArgs {file_path:&long,line:1,character:0},&TagTheme);assert!(rendered.contains("...:1:0</accent>"));}}
