use crate::{cli::{RunSgOptions, run_sg}, languages::CLI_LANGUAGES, pattern_hints::get_pattern_hint, result_formatter::{format_replace_result, format_search_result}};
use maho_ext_api::{ToolCall, ToolContent, ToolDefinition, ToolExecutionMode, ToolResult};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

pub fn tool_with_resolver(replace: bool, resolver: Arc<crate::binary_path::BinaryResolver>) -> ToolDefinition {
    tool_definition(replace, PathBuf::new(), Some(resolver))
}

pub fn tool(replace: bool, binary: PathBuf) -> ToolDefinition {
    tool_definition(replace, binary, None)
}

fn tool_definition(replace: bool, binary: PathBuf, resolver: Option<Arc<crate::binary_path::BinaryResolver>>) -> ToolDefinition {
    let mut properties = json!({
        "pattern":{"type":"string","description":if replace { "AST pattern to match" } else { "AST pattern with meta-variables ($VAR, $$$). Must be a complete AST node." }},
        "lang":{"type":"string","enum":CLI_LANGUAGES,"description":"Target language"},
        "paths":{"type":"array","items":{"type":"string"},"description":if replace { "Paths to search" } else { "Paths to search (default: current working directory)" }},
        "globs":{"type":"array","items":{"type":"string"},"description":"Include/exclude globs (prefix ! to exclude)"}
    });
    if replace {
        properties["rewrite"] = json!({"type":"string","description":"Replacement pattern (can use $VAR from pattern)"});
        properties["dryRun"] = json!({"type":"boolean","description":"Preview changes without applying (default: true)"});
    } else { properties["context"] = json!({"type":"number","description":"Number of context lines around each match"}); }
    let schema = json!({"type":"object","properties":properties,"required":if replace { vec!["pattern","rewrite","lang"] } else { vec!["pattern","lang"] }});
    let name = if replace { "ast_grep_replace" } else { "ast_grep_search" };
    let description = if replace {
        "Replace code patterns across the filesystem with AST-aware rewriting. Dry-run by default. Use meta-variables in `rewrite` to preserve matched content. Example: pattern='console.log($MSG)' rewrite='logger.info($MSG)'."
    } else {
        "Search code patterns across the filesystem using AST-aware matching. Use meta-variables: $VAR (single node), $$$ (multiple nodes). Patterns must be complete AST nodes (valid code). Examples: 'console.log($MSG)', 'def $FUNC($$$):', 'function $NAME($$$) { $$$ }'."
    };
    let mut definition = ToolDefinition::new(name, description, schema, Arc::new(move |call: ToolCall<'_>| {
        let binary = binary.clone();
        let resolver = resolver.clone();
        Box::pin(async move {
            let language = call.params.get("lang").and_then(Value::as_str).unwrap_or("undefined");
            if !CLI_LANGUAGES.contains(&language) { return Ok(ToolResult::text(format!("Unsupported language: {language}"))); }
            let pattern = call.params.get("pattern").and_then(Value::as_str).unwrap_or_default().to_owned();
            let strings = |key: &str| call.params.get(key).and_then(Value::as_array).map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();
            let mut paths = strings("paths");
            if paths.is_empty() { paths.push(call.context.map_or_else(|| ".".to_owned(), |ctx| ctx.cwd().to_string_lossy().into_owned())); }
            let dry_run = call.params.get("dryRun") != Some(&Value::Bool(false));
            let options = RunSgOptions { pattern: pattern.clone(), lang: language.to_owned(), paths: paths.clone(), globs: strings("globs"), rewrite: if replace { Some(call.params.get("rewrite").and_then(Value::as_str).unwrap_or_default().to_owned()) } else { None }, context: if replace { None } else { call.params.get("context").and_then(Value::as_f64) }, update_all: replace && !dry_run };
            let result = match resolver { Some(resolver) => crate::cli::run_sg_resolved(&options, &resolver).await, None => run_sg(&options, &binary).await };
            let hint = if !replace && result.matches.is_empty() && result.error.is_none() { get_pattern_hint(&pattern, language) } else { None };
            let mut text = if replace { format_replace_result(&result, dry_run) } else { format_search_result(&result) };
            if let Some(hint) = &hint { text.push_str(&format!("\n\n{hint}")); }
            let mut details = serde_json::to_value(&result)?;
            details["pattern"] = json!(pattern);
            details["lang"] = json!(language);
            details["paths"] = json!(paths);
            if let Some(globs) = call.params.get("globs") { details["globs"] = globs.clone(); }
            if let Some(hint) = hint { details["hint"] = json!(hint); }
            if replace { details["rewrite"] = json!(options.rewrite); details["dryRun"] = json!(dry_run); }
            Ok(ToolResult { content: vec![ToolContent::text(text)], details: Some(details) })
        })
    }));
    definition.label = if replace { "AST Grep Replace" } else { "AST Grep Search" }.into();
    if replace { definition.execution_mode = Some(ToolExecutionMode::Sequential); }
    definition.prompt_snippet = Some(if replace { "Rewrite code by AST pattern across 25 languages. Dry-run by default; pass dryRun=false to apply." } else { "Search code by AST structure across 25 languages using $VAR and $$$ meta-variables (NOT regex)." }.into());
    definition.prompt_guidelines = Some(if replace {
        vec!["Use ast_grep_replace dryRun=true first to preview changes; only set dryRun=false after confirming match list.".into(), "Use ast_grep_replace instead of edit when the rewrite spans many files with the same structural pattern.".into()]
    } else {
        vec!["Use ast_grep_search instead of grep when the pattern depends on code structure (function/class/import/call shape).".into(), "Use grep instead of ast_grep_search for plain text or cross-language regex search.".into(), "Run multiple ast_grep_search calls in parallel when checking different patterns.".into()]
    });
    definition
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn search_metadata() { let definition = tool(false, "sg".into()); assert_eq!(definition.name, "ast_grep_search"); assert_eq!(definition.label, "AST Grep Search"); }
    #[test] fn search_schema() { let definition = tool(false, "sg".into()); assert_eq!(definition.parameters["required"], json!(["pattern", "lang"])); for key in ["pattern", "lang", "paths", "globs", "context"] { assert!(definition.parameters["properties"].get(key).is_some()); } }
    #[test] fn replace_metadata() { let definition = tool(true, "sg".into()); assert_eq!(definition.name, "ast_grep_replace"); assert_eq!(definition.execution_mode, Some(ToolExecutionMode::Sequential)); }
    #[test] fn replace_schema() { let definition = tool(true, "sg".into()); assert_eq!(definition.parameters["required"], json!(["pattern", "rewrite", "lang"])); for key in ["pattern", "rewrite", "lang", "paths", "globs", "dryRun"] { assert!(definition.parameters["properties"].get(key).is_some()); } }
}
