pub struct TruncationResult { pub result: String, pub truncated: bool, pub original_bytes: usize, pub result_bytes: usize }
pub fn truncate_bytes(content: &str, max_bytes: usize) -> TruncationResult {
 let original_bytes = content.len();
 if original_bytes <= max_bytes { return TruncationResult { result: content.into(), truncated: false, original_bytes, result_bytes: original_bytes }; }
 let mut end = max_bytes;
 while !content.is_char_boundary(end) { end = end.saturating_sub(1); }
 let result = content[..end].trim_end_matches('�').to_owned();
 TruncationResult { result_bytes: result.len(), result, truncated: true, original_bytes }
}
