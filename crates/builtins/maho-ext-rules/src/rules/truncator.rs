use super::constants::TRUNCATION_NOTICE;
pub struct TruncationResult { pub body: String, pub truncated: bool, pub original_length: usize }
pub struct BudgetRule { pub body: String, pub relative_path: String }
pub struct BudgetResult { pub body: String, pub relative_path: String, pub truncated: bool }
fn utf16_length(value: &str) -> usize { value.encode_utf16().count() }
fn prefix(value: &str, max_units: usize) -> &str {
 let mut used: usize = 0; let mut end: usize = 0;
 for (offset, character) in value.char_indices() { let next = used.saturating_add(character.len_utf16()); if next > max_units { break; } used = next; end = offset.saturating_add(character.len_utf8()); }
 &value[..end]
}
pub fn truncate_rule(body: &str, max_chars: usize, relative_path: &str) -> TruncationResult {
 let original_length = utf16_length(body);
 if original_length <= max_chars { return TruncationResult { body: body.into(), truncated: false, original_length }; }
 let notice = TRUNCATION_NOTICE.replace("{path}", relative_path); let notice_length = utf16_length(&notice);
 let body = if max_chars < notice_length { notice } else { format!("{}{notice}", prefix(body, max_chars.saturating_sub(notice_length))) };
 TruncationResult { body, truncated: true, original_length }
}
pub fn truncate_budget(rules: &[BudgetRule], max_result_chars: usize) -> Vec<BudgetResult> {
 let mut results = Vec::new(); let mut remaining = max_result_chars;
 for rule in rules {
  let length = utf16_length(&rule.body);
  if remaining >= length { results.push(BudgetResult { body: rule.body.clone(), relative_path: rule.relative_path.clone(), truncated: false }); remaining = remaining.saturating_sub(length); continue; }
  let notice = TRUNCATION_NOTICE.replace("{path}", &rule.relative_path); let notice_length = utf16_length(&notice);
  if remaining <= notice_length { break; }
  let body = format!("{}{notice}", prefix(&rule.body, remaining.saturating_sub(notice_length)));
  remaining = remaining.saturating_sub(utf16_length(&body));
  results.push(BudgetResult { body, relative_path: rule.relative_path.clone(), truncated: true });
 }
 results
}
