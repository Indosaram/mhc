use maho_ext_api::{AgentToolResult, ToolRenderers};
use maho_tui::{components::text::Text, utils::truncate_to_width};
use serde_json::Value;
use std::sync::Arc;

fn string<'a>(value: &'a Value, key: &str) -> &'a str { value.get(key).and_then(Value::as_str).unwrap_or_default() }
fn shorten(path: &str) -> String {
    let path = path.replace('\\', "/");
    let home = std::env::var("HOME").unwrap_or_default().replace('\\', "/");
    let path = if !home.is_empty() { path.strip_prefix(&home).map(|rest|format!("~{rest}")).unwrap_or(path) } else { path };
    let units = path.encode_utf16().collect::<Vec<_>>();
    if units.len() > 42 { format!("…{}",String::from_utf16_lossy(&units[units.len()-41..])) } else if path.is_empty() { ".".into() } else { path }
}
fn truncate_error(message: &str) -> String {
    let units = message.encode_utf16().collect::<Vec<_>>();
    if units.len() <= 180 { message.into() } else { format!("{}…",String::from_utf16_lossy(&units[..179])) }
}
fn plural(count: usize, singular: &str, plural: &str) -> String { format!("{count} {}",if count == 1 { singular } else { plural }) }
fn strings<'a>(value: &'a Value, key: &str) -> Option<Vec<&'a str>> { value.get(key)?.as_array()?.iter().map(Value::as_str).collect() }
pub fn call_text(args: &Value, replace: bool) -> String {
    let paths = strings(args,"paths").unwrap_or_default();
    let mut path = shorten(paths.first().copied().unwrap_or("."));
    if paths.len() > 1 { path.push_str(&format!(" +{}",paths.len()-1)); }
    let pattern = string(args,"pattern");
    let mut text = if replace { format!("ast_grep_replace /{pattern}/ → {} in {path}",string(args,"rewrite")) } else { format!("ast_grep_search /{pattern}/ in {path}") };
    let lang = string(args,"lang");
    if !lang.is_empty() { text.push_str(&format!(" [{lang}]")); }
    if let Some(globs) = strings(args,"globs") && let Some(first) = globs.first() { text.push_str(&format!(" [glob {first}{}]",if globs.len()>1 { format!(" +{}",globs.len()-1) } else { String::new() })); }
    if replace { if args.get("dryRun") != Some(&Value::Bool(false)) { text.push_str(" [dry-run]"); } }
    else if let Some(context) = args.get("context").and_then(Value::as_f64) { text.push_str(&format!(" [context {context}]")); }
    text
}
fn valid_details(details: &Value, replace: bool) -> bool {
    details.get("pattern").is_some_and(Value::is_string) && details.get("lang").is_some_and(Value::is_string) && strings(details,"paths").is_some()
        && details.get("totalMatches").is_some_and(Value::is_number) && details.get("truncated").is_some_and(Value::is_boolean)
        && details.get("matches").and_then(Value::as_array).is_some_and(|matches|matches.iter().all(|item|item.get("file").is_some_and(Value::is_string) && item.get("text").is_some_and(Value::is_string) && item.get("lines").is_some_and(Value::is_string) && item.pointer("/range/start/line").is_some_and(Value::is_number) && item.pointer("/range/start/column").is_some_and(Value::is_number)))
        && ["error","hint"].iter().all(|key|details.get(key).is_none_or(Value::is_string))
        && details.get("truncatedReason").is_none_or(|value|matches!(value.as_str(),Some("max_matches"|"max_output_bytes"|"timeout")))
        && (!replace || (details.get("rewrite").is_some_and(Value::is_string) && details.get("dryRun").is_some_and(Value::is_boolean)))
}
pub fn result_text(result: &AgentToolResult, expanded: bool, is_error: bool, replace: bool) -> String {
    let details = &result.details;
    if !valid_details(details,replace) {
        let output = result.content.iter().find_map(|content|match content { maho_ext_api::ContentBlock::Text(content) => Some(content.text.trim()), _ => None }).unwrap_or_default();
        return if is_error && !output.is_empty() { format!("Error: {}",truncate_error(output)) } else if output.is_empty() { "No output".into() } else { output.into() };
    }
    let error = string(details,"error");
    if !error.is_empty() { return format!("Error: {}",truncate_error(error)); }
    let total = details["totalMatches"].as_f64().unwrap_or_default() as usize;
    if total == 0 { return if replace { "No matches found to replace".into() } else { let hint = string(details,"hint"); if hint.is_empty() { "No matches found".into() } else { format!("No matches found\n{hint}") } }; }
    let matches = details["matches"].as_array().expect("validated array");
    let mut groups: Vec<(&str,Vec<&Value>)> = Vec::new();
    for item in matches { let file = string(item,"file"); if let Some((_,matches)) = groups.iter_mut().find(|(existing,_)|*existing == file) { matches.push(item); } else { groups.push((file,vec![item])); } }
    let mut text = if replace { let replacements = plural(total,"replacement","replacements"); if details["dryRun"] == true { format!("[DRY RUN] {replacements} previewed") } else { format!("Applied {replacements}") } } else { plural(total,"match","matches") };
    text.push_str(&format!(" • {}",plural(groups.len(),"file","files")));
    if details["truncated"] == true {
        let reason = match string(details,"truncatedReason") { "max_matches" => "match limit reached", "max_output_bytes" => "output exceeded 1MB limit", "timeout" => "search timed out", _ => "results truncated" };
        text.push_str(&if expanded { format!("\n[Truncated: {reason}]") } else { format!(" [truncated: {reason}]") });
    }
    if !expanded {
        for (file,matches) in groups.iter().take(3) { text.push_str(&format!("\n  {} ({})",shorten(file),plural(matches.len(),"match","matches"))); }
        if groups.len()>3 { text.push_str(&format!("\n  … {} more files",groups.len()-3)); }
    } else {
        let mut lines = Vec::new(); let mut count = 0;
        for (file,matches) in groups {
            if count >= 15 { break; }
            lines.push(shorten(file));
            for item in matches {
                if count >= 15 { break; }
                let snippet = string(item,"lines").trim(); let snippet = if snippet.is_empty() { string(item,"text").trim() } else { snippet };
                lines.push(format!("  {}:{}  {}",item["range"]["start"]["line"].as_f64().unwrap_or_default()+1.0,item["range"]["start"]["column"].as_f64().unwrap_or_default()+1.0,truncate_to_width(snippet,160,"…",false)));
                count += 1;
            }
        }
        if total>count { lines.push(format!("… {} more matches not shown",total-count)); }
        if !lines.is_empty() { text.push_str(&format!("\n\n{}",lines.join("\n"))); }
    }
    text
}
pub fn renderers(replace: bool) -> ToolRenderers<(),Value> {
    ToolRenderers { render_call: Some(Arc::new(move |args,_,_|Box::new(Text::with_padding(call_text(args,replace),0,0)))),
        render_result: Some(Arc::new(move |result,options,_,context|Box::new(Text::with_padding(result_text(result,options.expanded,context.is_error,replace),0,0)))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> AgentToolResult {
        serde_json::from_value(json!({"content":[{"type":"text","text":""}],"details":{"pattern":"console.$METHOD($$$)","lang":"typescript","paths":["src"],"matches":[
            {"file":"src/logger.ts","lines":"console.log(message);","text":"console.log(message);","range":{"start":{"line":0,"column":0}}},
            {"file":"src/console.ts","lines":"console.error(message);","text":"console.error(message);","range":{"start":{"line":7,"column":1}}},
            {"file":"src/logger.ts","lines":"console.warn(message);","text":"console.warn(message);","range":{"start":{"line":12,"column":2}}}],"totalMatches":3,"truncated":false}})).unwrap()
    }
    #[test] fn collapsed_groups_match_reference() { let output = result_text(&fixture(),false,false,false); for expected in ["3 matches","2 files","src/logger.ts (2 matches)","src/console.ts (1 match)"] { assert!(output.contains(expected)); } }
    #[test] fn expanded_locations_match_reference() { let output = result_text(&fixture(),true,false,false); for expected in ["3 matches","2 files","1:1","13:3","8:2","console.error(message);"] { assert!(output.contains(expected)); } assert!(output.find("13:3").unwrap()<output.find("8:2").unwrap()); }
    #[test] fn byte_truncation_matches_reference() { let mut result = fixture(); result.details["truncated"] = json!(true); result.details["truncatedReason"] = json!("max_output_bytes"); result.details["totalMatches"] = json!(15); let output = result_text(&result,false,false,false); assert!(output.contains("output exceeded 1MB limit")); assert!(!output.contains("max_output_bytes")); }
    #[test] fn fallback_error_remains_visible() { let mut result = fixture(); result.details = json!({}); result.content = serde_json::from_value(json!([{"type":"text","text":"Output too large and could not be parsed"}])).unwrap(); assert_eq!(result_text(&result,false,true,false),"Error: Output too large and could not be parsed"); }
    #[test] fn dry_run_replacement_matches_reference() { let mut result = fixture(); result.details["rewrite"] = json!("logger.info($MSG)"); result.details["dryRun"] = json!(true); let output = result_text(&result,false,false,true); assert!(output.contains("[DRY RUN] 3 replacements previewed")); assert!(output.contains("2 files")); }
}
