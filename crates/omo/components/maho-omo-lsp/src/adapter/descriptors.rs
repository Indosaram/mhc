use std::sync::Arc;
use maho_ext_api::{ToolDefinition,ToolError,ToolExecutionMode};
use serde_json::{Value,json};
use super::schema::*;
pub const TOOL_NAMES:&[&str]=&["lsp_diagnostics","lsp_goto_definition","lsp_find_references","lsp_symbols","lsp_prepare_rename","lsp_rename"];
pub fn descriptors() -> Vec<ToolDefinition> {
    TOOL_NAMES.iter().map(|&name| {
        let (label,description,parameters)=match name {
            "lsp_diagnostics"=>("LSP Diagnostics","Get errors, warnings, and hints from the language server BEFORE running build. Works for both single files and directories - file extension is auto-detected for directories.",object(json!({"filePath":string("File or directory path to check diagnostics for"),"severity":optional(union(&["error","warning","information","hint","all"],"Filter by severity level"))}))),
            "lsp_goto_definition"=>("LSP Goto Definition","Jump to symbol definition. Find WHERE something is defined.",object(json!({"filePath":string("Path to the source file containing the symbol"),"line":number("1-based line number of the symbol"),"character":number("0-based column of the symbol on that line")}))),
            "lsp_find_references"=>("LSP Find References","Find ALL usages/references of a symbol across the entire workspace.",object(json!({"filePath":string("Path to the source file"),"line":number("1-based line of the symbol"),"character":number("0-based column of the symbol on that line"),"includeDeclaration":optional(boolean("Include the declaration itself (default: true)"))}))),
            "lsp_symbols"=>("LSP Symbols","Get symbols from a file (document) or search across the workspace. Use scope='document' for a file outline, scope='workspace' for project-wide symbol search.",object(json!({"filePath":string("File path used as LSP context"),"scope":union(&["document","workspace"],"'document' for file symbols, 'workspace' for project-wide search"),"query":optional(string("Symbol name to search (required for workspace scope)")),"limit":optional(number("Max results (default: 200)"))}))),
            "lsp_prepare_rename"=>("LSP Prepare Rename","Check if rename is valid at a given position. Use BEFORE lsp_rename.",position(false)),
            "lsp_rename"=>("LSP Rename","Rename symbol across the entire workspace. APPLIES changes to all files.",position(true)),
            _=>unreachable!("fixed descriptor names"),
        };
        let mut tool=ToolDefinition::new(name,description,parameters,Arc::new(|_|Box::pin(async {Err(ToolError::Message("Senpi LSP descriptors must be wrapped with the packaged daemon runtime before execution".into()))})));
        tool.label=label.into();
        if name=="lsp_rename" {tool.execution_mode=Some(ToolExecutionMode::Sequential);}
        tool
    }).collect()
}
fn position(rename:bool) -> Value {let mut p=json!({"filePath":string("Path to the source file"),"line":number("1-based line of the symbol"),"character":number("0-based column of the symbol on that line")});if rename {p["newName"]=string("New symbol name");}object(p)}
#[cfg(test)]
mod tests {use super::*;#[test] fn six_descriptors() {let tools=descriptors();assert_eq!(tools.iter().map(|t|t.name.as_str()).collect::<Vec<_>>(),TOOL_NAMES);assert_eq!(tools.last().unwrap().execution_mode,Some(ToolExecutionMode::Sequential));assert!(tools[..5].iter().all(|t|t.execution_mode.is_none()));}#[test] fn required_schema_fields() {let tools=descriptors();assert_eq!(tools[0].parameters["required"],json!(["filePath"]));assert!(tools[5].parameters["required"].as_array().unwrap().contains(&json!("newName")));assert!(!tools[2].parameters["required"].as_array().unwrap().contains(&json!("includeDeclaration")));}}
