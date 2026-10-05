use crate::{stream_utils::{ScalarEntry, CharCode, is_ascii_whitespace}, types::{DetectorMatch, DetectorRule, DetailValue}};
pub const CJK_SCALAR_RUN_THRESHOLD: u32 = 224;
pub const PUNCTUATION_RUN_THRESHOLD: u32 = 256;
pub const RUN_WHITESPACE_MAX_CHARS: u32 = 32;
pub const RUN_WHITESPACE_TARGET_RATIO: u32 = 8;
pub const NEWLINE_FLOOD_MIN_CHARS: u32 = 320;
pub const NEWLINE_FLOOD_MIN_NEWLINES: u32 = 8;
pub const GENERIC_FLOOD_MIN_CHARS: u32 = 480;
fn code_point_of(entry: &ScalarEntry) -> u32 { match entry.value.as_slice() { [high, low, ..] if (0xd800..=0xdbff).contains(high) && (0xdc00..=0xdfff).contains(low) => 0x10000 + ((u32::from(*high) - 0xd800) << 10) + u32::from(*low) - 0xdc00, [code, ..] => u32::from(*code), [] => 0 } }
pub const fn is_ascii_alphanumeric(code: u32) -> bool { (code >= 48 && code <= 57) || (code >= 65 && code <= 90) || (code >= 97 && code <= 122) }
pub const fn is_box_drawing(code: u32) -> bool { code >= 0x2500 && code <= 0x259f }
const fn non_decorative_punctuation(code: u32) -> bool { matches!(code, 33 | 36 | 37 | 38 | 63 | 64) }
pub const fn is_decorative_scalar(code: u32) -> bool { if code <= 0x7f { code > 32 && code <= 0x7e && !is_ascii_alphanumeric(code) && !non_decorative_punctuation(code) } else { is_box_drawing(code) } }
fn run_threshold_for(entry: &ScalarEntry) -> u32 { let code = code_point_of(entry); if code > 0x7f { if is_box_drawing(code) { 0 } else { CJK_SCALAR_RUN_THRESHOLD } } else if non_decorative_punctuation(code) { PUNCTUATION_RUN_THRESHOLD } else { 0 } }
#[derive(Default)]
pub struct DominantRunState { pub active: bool, pub target: Vec<u16>, pub target_width: usize, pub threshold: u32, pub count: u32, pub whitespace_used: u32, pub start_offset: usize }
pub fn create_dominant_run_state() -> DominantRunState { DominantRunState::default() }
pub fn update_dominant_run(state: &mut DominantRunState, entry: &ScalarEntry) -> Option<DetectorMatch> {
    let code = code_point_of(entry);
    if u16::try_from(code).is_ok_and(is_ascii_whitespace) {
        if !state.active { return None; }
        let next = state.whitespace_used + 1;
        if next > RUN_WHITESPACE_MAX_CHARS || next * RUN_WHITESPACE_TARGET_RATIO > state.count { state.active = false; return None; }
        state.whitespace_used = next; return None;
    }
    let threshold = run_threshold_for(entry);
    if threshold == 0 { state.active = false; return None; }
    if !state.active || state.target != entry.value { state.active = true; state.target = entry.value.clone(); state.target_width = entry.width; state.threshold = threshold; state.count = 1; state.whitespace_used = 0; state.start_offset = entry.start_offset; return None; }
    state.count += 1;
    if state.count < state.threshold { return None; }
    Some(DetectorMatch { rule: DetectorRule::CollapseRepetition, reason: format!("dominant scalar run U+{code:x} repeated {} times", state.count), anomaly_start_offset: state.start_offset, garbage_start_offset: state.start_offset + state.target_width, detail: [("mechanism".into(), DetailValue::String("dominant-scalar-run".into())), ("codePoint".into(), DetailValue::Number(f64::from(code))), ("count".into(), DetailValue::Number(f64::from(state.count))), ("threshold".into(), DetailValue::Number(f64::from(state.threshold)))].into() })
}
#[derive(Default)]
pub struct WhitespaceFloodState { pub count: u32, pub newlines: u32, pub start_offset: usize }
pub fn create_whitespace_flood_state() -> WhitespaceFloodState { WhitespaceFloodState::default() }
pub fn update_whitespace_flood(state: &mut WhitespaceFloodState, entry: &ScalarEntry) -> Option<DetectorMatch> {
    let code = code_point_of(entry);
    if !u16::try_from(code).is_ok_and(is_ascii_whitespace) { state.count = 0; state.newlines = 0; return None; }
    if state.count == 0 { state.start_offset = entry.start_offset; }
    state.count += 1;
    if code == u32::from(CharCode::LINE_FEED) { state.newlines += 1; }
    if !(state.count >= NEWLINE_FLOOD_MIN_CHARS && state.newlines >= NEWLINE_FLOOD_MIN_NEWLINES || state.count >= GENERIC_FLOOD_MIN_CHARS) { return None; }
    Some(DetectorMatch { rule: DetectorRule::CollapseRepetition, reason: format!("whitespace flood of {} characters with {} newlines", state.count, state.newlines), anomaly_start_offset: state.start_offset, garbage_start_offset: state.start_offset + 1, detail: [("mechanism".into(), DetailValue::String("whitespace-flood".into())), ("whitespaceChars".into(), DetailValue::Number(f64::from(state.count))), ("newlines".into(), DetailValue::Number(f64::from(state.newlines)))].into() })
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::stream_utils::ScalarScanner;
    fn feed_run(text: &str) -> Option<DetectorMatch> { let mut state = create_dominant_run_state(); ScalarScanner::default().push(&text.encode_utf16().collect::<Vec<_>>()).iter().find_map(|e| update_dominant_run(&mut state, e)) }
    fn feed_flood(text: &str) -> Option<DetectorMatch> { let mut state = create_whitespace_flood_state(); ScalarScanner::default().push(&text.encode_utf16().collect::<Vec<_>>()).iter().find_map(|e| update_whitespace_flood(&mut state, e)) }
    #[test] fn punctuation_run_fires_at_256() { let input = "!".repeat(256); let result = feed_run(&input).unwrap(); assert_eq!(result.garbage_start_offset, 1); assert_eq!(result.detail["count"], DetailValue::Number(256.0)); }
    #[test] fn cjk_run_fires_at_224() { let input = "永".repeat(224); let result = feed_run(&input).unwrap(); assert_eq!(result.garbage_start_offset, 1); }
    #[test] fn supplementary_scalar_keeps_utf16_width() { let input = "😀".repeat(224); let result = feed_run(&input).unwrap(); assert_eq!(result.garbage_start_offset, 2); }
    #[test] fn punctuation_below_threshold_is_allowed() { let input = "!".repeat(255); let result = feed_run(&input); assert!(result.is_none()); }
    #[test] fn decorative_ascii_separator_is_allowed() { let input = "=".repeat(2000); let result = feed_run(&input); assert!(result.is_none()); }
    #[test] fn box_drawing_is_allowed() { let input = "─".repeat(2000); let result = feed_run(&input); assert!(result.is_none()); }
    #[test] fn ascii_letter_run_is_allowed() { let input = "A".repeat(5000); let result = feed_run(&input); assert!(result.is_none()); }
    #[test] fn spaces_flood_at_480() { let input = " ".repeat(480); let result = feed_flood(&input).unwrap(); assert_eq!(result.detail["whitespaceChars"], DetailValue::Number(480.0)); }
    #[test] fn newlines_flood_at_320() { let input = "\n".repeat(320); let result = feed_flood(&input).unwrap(); assert_eq!(result.detail["whitespaceChars"], DetailValue::Number(320.0)); }
    #[test] fn non_whitespace_breaks_flood() { let input = format!("{}x{}", " ".repeat(300), " ".repeat(300)); let result = feed_flood(&input); assert!(result.is_none()); }
    #[test] fn limited_interleaved_whitespace_preserves_run() { let input = format!("{} {}", "!".repeat(100), "!".repeat(156)); let result = feed_run(&input); assert!(result.is_some()); }
    #[test] fn early_whitespace_breaks_run() { let input = format!("! {}", "!".repeat(255)); let result = feed_run(&input); assert!(result.is_none()); }
}
