use crate::types::{CliMatch, SgResult, TruncationReason};

pub fn create_sg_result_from_stdout(stdout: &str) -> SgResult {
    if stdout.trim().is_empty() { return SgResult::default(); }
    let truncated = stdout.encode_utf16().count() >= 1024 * 1024;
    let prefix = if truncated { String::from_utf16_lossy(&stdout.encode_utf16().take(1024 * 1024).collect::<Vec<_>>()) } else { stdout.to_owned() };
    let parsed = serde_json::from_str::<serde_json::Value>(&prefix);
    let mut matches = match parsed {
        Ok(value) => serde_json::from_value::<Vec<CliMatch>>(value).unwrap_or_default(),
        Err(_) if !truncated => return SgResult::default(),
        Err(_) => {
            let salvaged = prefix.rfind('}').and_then(|end| prefix.rmatch_indices("},").find(|(start, _)| *start <= end).map(|(start, _)| start)).filter(|end| *end > 0).and_then(|end| serde_json::from_str::<Vec<CliMatch>>(&format!("{}]", &prefix[..=end])).ok());
            match salvaged {
                Some(value) => value,
                None => return SgResult { truncated: true, truncated_reason: Some(TruncationReason::MaxOutputBytes), error: Some("Output too large and could not be parsed".into()), ..Default::default() },
            }
        }
    };
    let total_matches = matches.len();
    let match_limit = total_matches > 500;
    matches.truncate(500);
    SgResult { matches, total_matches, truncated: truncated || match_limit, truncated_reason: if truncated { Some(TruncationReason::MaxOutputBytes) } else if match_limit { Some(TruncationReason::MaxMatches) } else { None }, error: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn matched(file: &str) -> serde_json::Value {
        serde_json::json!({"text":"console.log(value)","file":file,"lines":"console.log(value);","language":"TypeScript","range":{"start":{"line":0,"column":0},"end":{"line":0,"column":18},"byteOffset":{"start":0,"end":18}},"charCount":{"leading":0,"trailing":1}})
    }
    #[test] fn empty_when_empty() { let result = create_sg_result_from_stdout(""); assert_eq!(result.total_matches, 0); assert!(!result.truncated); }
    #[test] fn preserved_when_valid_matches() { let stdout = serde_json::to_string(&vec![matched("one.ts"), matched("two.ts")]).expect("serialize fixture"); let result = create_sg_result_from_stdout(&stdout); assert_eq!(result.matches.len(), 2); assert_eq!(result.matches[1].file, "two.ts"); assert!(!result.truncated); }
    #[test] fn limited_when_more_than_500_matches() { let stdout = serde_json::to_string(&vec![matched("one.ts"); 501]).expect("serialize many matches"); let result = create_sg_result_from_stdout(&stdout); assert_eq!(result.total_matches, 501); assert_eq!(result.matches.len(), 500); assert!(matches!(result.truncated_reason, Some(TruncationReason::MaxMatches))); }
    #[test] fn salvaged_when_complete_objects_precede_truncation() { let prefix = format!("[{}, {},{{\"text\":\"", matched("one.ts"), matched("two.ts")); let stdout = format!("{prefix}{}", "x".repeat(1024 * 1024)); let result = create_sg_result_from_stdout(&stdout); assert_eq!(result.matches.len(), 2); assert!(matches!(result.truncated_reason, Some(TruncationReason::MaxOutputBytes))); assert!(result.error.is_none()); }
    #[test] fn empty_when_blank() { assert!(create_sg_result_from_stdout(" \n").matches.is_empty()); }
    #[test] fn empty_when_invalid_json() { assert!(!create_sg_result_from_stdout("invalid").truncated); }
    #[test] fn empty_when_wrong_shape() { assert!(create_sg_result_from_stdout("[{}]").matches.is_empty()); }
    #[test] fn error_when_large_unparseable() { let result = create_sg_result_from_stdout(&"x".repeat(1024 * 1024)); assert!(result.truncated); assert!(result.error.is_some()); }
    #[test] fn error_when_salvaged_array_has_invalid_matches() { let text = format!("[{{}},{{\"text\":\"{}", "x".repeat(1024 * 1024)); let result = create_sg_result_from_stdout(&text); assert!(result.error.is_some()); }
}
