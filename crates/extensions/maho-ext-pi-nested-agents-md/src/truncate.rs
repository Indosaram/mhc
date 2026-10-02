#[derive(Debug, PartialEq, Eq)]
pub struct TruncationResult {
    pub result: String,
    pub truncated: bool,
    pub original_bytes: usize,
    pub result_bytes: usize,
}

pub fn truncate_bytes(content: &str, max_bytes: usize) -> TruncationResult {
    let truncated = content.len() > max_bytes;
    let mut end = content.len().min(max_bytes);
    while !content.is_char_boundary(end) { end -= 1; }
    let result = if truncated { content[..end].trim_end_matches('\u{fffd}').to_owned() } else { content.to_owned() };
    TruncationResult { original_bytes: content.len(), result_bytes: result.len(), result, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn unchanged_when_below_budget() { let result = truncate_bytes("hello world", 1024); assert_eq!(result.result, "hello world"); assert!(!result.truncated); assert_eq!(result.result_bytes, 11); }
    #[test] fn unchanged_when_at_budget() { assert!(!truncate_bytes("abcdefghij", 10).truncated); }
    #[test] fn truncated_when_above_budget() { let result = truncate_bytes(&"a".repeat(2048), 1024); assert!(result.truncated); assert_eq!(result.result_bytes, 1024); assert_eq!(result.original_bytes, 2048); }
    #[test] fn intact_when_four_byte_boundary() { assert_eq!(truncate_bytes("aaaaaaaaaa\u{1f496}bbbbbbbbbb", 12).result, "aaaaaaaaaa"); }
    #[test] fn unchanged_when_empty() { assert_eq!(truncate_bytes("", 16).result_bytes, 0); }
    #[test] fn intact_when_two_byte_boundary() { assert_eq!(truncate_bytes("ééééééééé", 5).result, "éé"); }
    #[test] fn intact_when_three_byte_boundary() { assert_eq!(truncate_bytes("あいあいあい", 8).result, "あい"); }
}
