use super::index::GrepToolDetails;
use crate::{definition::ToolContent, model_only_text::model_only_text};
pub fn display_path(path: &str) -> String {
    if path.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}') { serde_json::Value::String(path.into()).to_string() } else { path.into() }
}
pub fn format_grep_content(result: &GrepToolDetails) -> Vec<ToolContent> {
    let mut blocks = Vec::new();
    if !result.matches.is_empty() {
        for file in &result.file_matches {
            let mut lines = vec![display_path(&file.path)];
            lines.extend(result.matches.iter().filter(|m| m.path == file.path).map(|r| format!("{}{} {}", r.line, if r.is_context { '-' } else { ':' }, r.text)));
            blocks.push(lines.join("\n"));
        }
    } else if result.file_count > 0 {
        blocks.push(result.file_matches.iter().map(|f| match f.count { Some(n) => format!("{}: {n}", display_path(&f.path)), None => display_path(&f.path) }).collect::<Vec<_>>().join("\n"));
    } else { blocks.push(match result.status.as_str() { "pageEnd" => format!("No more results (skip={})", result.skip), "partial" => "No matches found in searched portion".into(), _ => "No matches found".into() }); }
    let scan = &result.scan; let mut notes = Vec::new();
    for (key,prefix,suffix) in [
        ("prefixSearched","Partial coverage: searched only the first 4 MiB of "," large file(s); later matches are omitted."),
        ("skippedOversized","Skipped "," unreadable/unsearchable large file(s)."),
        ("skippedBinary","Skipped "," binary file(s).")
    ] { if let Some(n) = scan[key].as_u64().filter(|n| *n > 0) { notes.push(format!("{prefix}{n}{suffix}")); } }
    if let Some(paths) = scan["missingPaths"].as_array().filter(|v| !v.is_empty()) { notes.push(format!("Skipped missing path(s): {}", paths.iter().filter_map(|p| p.as_str()).map(display_path).collect::<Vec<_>>().join(", "))); }
    if scan["timedOut"] == true {
        let warning = scan["warnings"].as_array().and_then(|ws| ws.iter().find(|w| w["code"] == "TIMED_OUT")).and_then(|w| w["message"].as_str());
        notes.push(warning.unwrap_or("Timed out after 30000 ms; showing the completed ordered prefix.").into());
    }
    if scan["regexEngine"] == "pcre2" { notes.push("Regex unsupported by the native engine; matched with ripgrep --pcre2.".into()); }
    match scan["patternKind"].as_str() { Some("literal") => notes.push(format!("Pattern searched literally: {}", scan["effectivePattern"].as_str().unwrap_or_default())), Some("sanitized") => notes.push(format!("Pattern sanitized: {}", scan["effectivePattern"].as_str().unwrap_or_default())), _ => {} }
    let footer = format!("[grep: matches={} files={} searched={} elapsedMs={:.0} engine={} nextSkip={}]", result.match_count.map_or("n/a".into(), |n| n.to_string()), result.file_count, scan["filesSearched"], scan["elapsedMs"].as_f64().unwrap_or(0.0).max(0.0), result.engine, result.next_skip.map_or("none".into(), |n| n.to_string()));
    vec![ToolContent::text(format!("{}\n{}", blocks.join("\n\n"), if notes.is_empty() { String::new() } else { format!("\n{}", notes.join("\n")) })), model_only_text(footer)]
}
