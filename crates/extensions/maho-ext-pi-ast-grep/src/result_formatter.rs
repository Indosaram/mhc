use crate::types::{SgResult, TruncationReason};

fn reason(result: &SgResult) -> String {
    match result.truncated_reason {
        Some(TruncationReason::MaxMatches) => format!("showing first {} of {}", result.matches.len(), result.total_matches),
        Some(TruncationReason::MaxOutputBytes) => "output exceeded 1MB limit".to_owned(),
        Some(TruncationReason::Timeout) | None => "search timed out".to_owned(),
    }
}

pub fn format_search_result(result: &SgResult) -> String {
    if let Some(error) = result.error.as_deref().filter(|error| !error.is_empty()) { return format!("Error: {error}"); }
    if result.matches.is_empty() { return "No matches found".to_owned(); }
    let mut lines = Vec::new();
    if result.truncated { lines.push(format!("[TRUNCATED] Results truncated ({})\n", reason(result))); }
    let suffix = if result.truncated { format!(" (truncated from {})", result.total_matches) } else { String::new() };
    lines.push(format!("Found {} match(es){suffix}:\n", result.matches.len()));
    for item in &result.matches {
        lines.push(format!("{}:{}:{}", item.file, item.range.start.line + 1.0, item.range.start.column + 1.0));
        lines.push(format!("  {}", item.lines.trim()));
        lines.push(String::new());
    }
    lines.join("\n")
}

pub fn format_replace_result(result: &SgResult, dry_run: bool) -> String {
    if let Some(error) = result.error.as_deref().filter(|error| !error.is_empty()) { return format!("Error: {error}"); }
    if result.matches.is_empty() { return "No matches found to replace".to_owned(); }
    let mut lines = Vec::new();
    if result.truncated { lines.push(format!("[TRUNCATED] Results truncated ({})\n", reason(result))); }
    let prefix = if dry_run { "[DRY RUN] " } else { "" };
    lines.push(format!("{prefix}{} replacement(s):\n", result.matches.len()));
    for item in &result.matches {
        lines.push(format!("{}:{}:{}", item.file, item.range.start.line + 1.0, item.range.start.column + 1.0));
        lines.push(format!("  {}", item.text));
        lines.push(String::new());
    }
    if dry_run { lines.push("Use dryRun=false to apply changes".to_owned()); }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    fn result() -> SgResult { SgResult { matches: vec![CliMatch { text: "x".into(), range: Range { start: Position { line: 4.0, column: 10.0 }, end: Position { line: 4.0, column: 20.0 }, byte_offset: ByteOffset { start: 0.0, end: 10.0 } }, file: "file.ts".into(), lines: " x ".into(), char_count: CharCount { leading: 0.0, trailing: 0.0 }, language: "typescript".into() }], total_matches: 1, ..Default::default() } }
    #[test] fn error_when_result_failed() { let value = SgResult { error: Some("boom".into()), ..Default::default() }; assert_eq!(format_search_result(&value), "Error: boom"); }
    #[test] fn empty_when_no_matches() { assert_eq!(format_search_result(&SgResult::default()), "No matches found"); }
    #[test] fn one_based_when_location_formatted() { assert!(format_search_result(&result()).contains("file.ts:5:11")); }
    #[test] fn count_when_match_limit() { let value = SgResult { truncated: true, total_matches: 7, truncated_reason: Some(TruncationReason::MaxMatches), ..result() }; assert!(format_search_result(&value).contains("showing first 1 of 7")); }
    #[test] fn output_when_byte_limit() { let value = SgResult { truncated: true, truncated_reason: Some(TruncationReason::MaxOutputBytes), ..result() }; assert!(format_search_result(&value).contains("output exceeded 1MB limit")); }
    #[test] fn timeout_when_time_limit() { let value = SgResult { truncated: true, truncated_reason: Some(TruncationReason::Timeout), ..result() }; assert!(format_search_result(&value).contains("search timed out")); }
    #[test] fn dry_run_when_preview() { assert!(format_replace_result(&result(), true).starts_with("[DRY RUN]")); }
    #[test] fn applied_when_not_preview() { assert!(!format_replace_result(&result(), false).contains("dryRun")); }
}
