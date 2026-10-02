pub fn collect_lines(text:&str,limit:usize)->Vec<String> { text.split('\n').take(limit).map(String::from).collect() }
pub fn collect_non_empty_trimmed_lines(text:&str,limit:usize)->Vec<String> { text.split('\n').map(|line|line.trim_matches(|character:char|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))).filter(|line|!line.is_empty()).take(limit).map(String::from).collect() }
pub fn format_bytes(bytes:usize)->String { if bytes<1024 { format!("{bytes} B") } else if bytes<1024*1024 { format!("{:.1} KB",bytes as f64/1024.0) } else { format!("{:.1} MB",bytes as f64/(1024.0*1024.0)) } }
pub fn is_progress_details(details:&serde_json::Value)->bool { matches!(details.get("phase").and_then(serde_json::Value::as_str),Some("fetching"|"downloading"|"converting")) }
pub fn is_result_details(details:&serde_json::Value)->bool { details.get("status").is_some_and(serde_json::Value::is_number) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn line_collection_preserves_final_empty_line() { assert_eq!(collect_lines("a\n",24),["a",""]); assert_eq!(collect_lines("",24),[""]); assert!(collect_lines("a",0).is_empty()); }
    #[test] fn preview_skips_blanks_and_stops_at_limit() { assert_eq!(collect_non_empty_trimmed_lines(" \n a \n\n b\nc",2),["a","b"]); }
    #[test] fn detail_guards_accept_only_machine_fields() { assert!(is_progress_details(&serde_json::json!({"phase":"downloading"}))); assert!(!is_progress_details(&serde_json::json!({"phase":"other"}))); assert!(is_result_details(&serde_json::json!({"status":404}))); assert!(!is_result_details(&serde_json::json!({"status":"404"}))); }
}
