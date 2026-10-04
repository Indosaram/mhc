use crate::{stream_utils::{CharCode, FixedRing, ScalarEntry, is_ascii_whitespace}, types::{DetectorMatch, DetectorRule, DetailValue}};
use super::collapse_scalars::{is_ascii_alphanumeric, is_box_drawing};
pub const PARAGRAPH_MIN_CHARS: usize = 64;
pub const PARAGRAPH_MIN_WORD_CHARS: u32 = 24;
pub const PARAGRAPH_REPEAT_THRESHOLD: u32 = 3;
pub const PARAGRAPH_RING_CAPACITY: usize = 64;
pub const PARAGRAPH_TEXT_RETENTION_MAX: usize = 512;
const HASH_A_OFFSET: u32 = 0x811c9dc5;
const HASH_B_OFFSET: u32 = 5381;
struct ParagraphEntry { hash_a: u32, hash_b: u32, utf16_length: usize, start_offset: usize, text: Option<Vec<u16>> }
pub struct ParagraphRepeatState {
    ring: FixedRing<ParagraphEntry>, line_hash_a: u32, line_hash_b: u32, line_length: usize, line_word_chars: u32, line_has_content: bool, line_start_offset: usize, line_text: Vec<u16>,
    hash_a: u32, hash_b: u32, length: usize, word_chars: u32, start_offset: usize, retained: Vec<u16>,
}
pub fn create_paragraph_repeat_state() -> ParagraphRepeatState { ParagraphRepeatState { ring: FixedRing::new(PARAGRAPH_RING_CAPACITY), line_hash_a: HASH_A_OFFSET, line_hash_b: HASH_B_OFFSET, line_length: 0, line_word_chars: 0, line_has_content: false, line_start_offset: 0, line_text: Vec::new(), hash_a: HASH_A_OFFSET, hash_b: HASH_B_OFFSET, length: 0, word_chars: 0, start_offset: 0, retained: Vec::new() } }
fn complete_paragraph(state: &mut ParagraphRepeatState) -> Option<DetectorMatch> {
    let paragraph = ParagraphEntry { hash_a: state.hash_a, hash_b: state.hash_b, utf16_length: state.length, start_offset: state.start_offset, text: (!state.retained.is_empty()).then(|| state.retained.clone()) };
    let eligible = state.length >= PARAGRAPH_MIN_CHARS && state.word_chars >= PARAGRAPH_MIN_WORD_CHARS;
    state.hash_a = HASH_A_OFFSET; state.hash_b = HASH_B_OFFSET; state.length = 0; state.word_chars = 0; state.start_offset = 0; state.retained.clear();
    if !eligible { return None; }
    let mut occurrences = 1_u32;
    let mut first = None;
    let mut second = None;
    for back in (0..state.ring.size()).rev() {
        let Some(previous) = state.ring.get_back(back) else { continue; };
        if previous.hash_a != paragraph.hash_a || previous.hash_b != paragraph.hash_b || previous.utf16_length != paragraph.utf16_length { continue; }
        occurrences += 1;
        if first.is_none() { first = Some((previous.start_offset, previous.text.clone())); } else if second.is_none() { second = Some(previous.start_offset); }
    }
    let length = paragraph.utf16_length;
    state.ring.push(paragraph);
    let (first_offset, text) = first?;
    let second_offset = second?;
    if occurrences < PARAGRAPH_REPEAT_THRESHOLD { return None; }
    let sample = text.map(|text| String::from_utf16_lossy(&text[..text.len().min(80)])).unwrap_or_default();
    Some(DetectorMatch { rule: DetectorRule::CollapseRepetition, reason: format!("paragraph repeated {occurrences} times within one message ({length} chars)"), anomaly_start_offset: first_offset, garbage_start_offset: second_offset, detail: [("mechanism".into(), DetailValue::String("paragraph-repeat".into())), ("occurrences".into(), DetailValue::Number(f64::from(occurrences))), ("paragraphChars".into(), DetailValue::Number(length as f64)), ("sample".into(), DetailValue::String(sample))].into() })
}
pub fn update_paragraph_repeats(state: &mut ParagraphRepeatState, entry: &ScalarEntry) -> Option<DetectorMatch> {
    if entry.value.first() == Some(&CharCode::LINE_FEED) {
        let result = if state.line_has_content {
            if state.length == 0 { state.start_offset = state.line_start_offset; }
            state.hash_a = (state.hash_a ^ state.line_hash_a).wrapping_mul(0x01000193); state.hash_b = state.hash_b.wrapping_mul(31).wrapping_add(state.line_hash_b);
            state.length += state.line_length + 1; state.word_chars += state.line_word_chars;
            if state.retained.len() < PARAGRAPH_TEXT_RETENTION_MAX { state.retained.extend_from_slice(&state.line_text); state.retained.push(CharCode::LINE_FEED); }
            None
        } else if state.length > 0 { complete_paragraph(state) } else { None };
        state.line_hash_a = HASH_A_OFFSET; state.line_hash_b = HASH_B_OFFSET; state.line_length = 0; state.line_word_chars = 0; state.line_has_content = false; state.line_start_offset = entry.start_offset + 1; state.line_text.clear();
        return result;
    }
    for code in &entry.value { state.line_hash_a = (state.line_hash_a ^ u32::from(*code)).wrapping_mul(0x01000193); state.line_hash_b = state.line_hash_b.wrapping_mul(31).wrapping_add(u32::from(*code)); }
    let code = match entry.value.as_slice() { [high, low, ..] if (0xd800..=0xdbff).contains(high) && (0xdc00..=0xdfff).contains(low) => 0x10000 + ((u32::from(*high) - 0xd800) << 10) + u32::from(*low) - 0xdc00, [code, ..] => u32::from(*code), [] => 0 };
    if !u16::try_from(code).is_ok_and(is_ascii_whitespace) { state.line_has_content = true; }
    if is_ascii_alphanumeric(code) || code > 0x7f && !is_box_drawing(code) { state.line_word_chars += 1; }
    state.line_length += entry.width;
    if state.line_text.len() < PARAGRAPH_TEXT_RETENTION_MAX { state.line_text.extend_from_slice(&entry.value); }
    None
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::stream_utils::ScalarScanner;
    #[test] fn paragraph_length_detail_is_not_saturated_to_u32() {
        let mut state=create_paragraph_repeat_state(); let length=u32::MAX as usize+100;
        for offset in [0,10,20] {
            state.length=length; state.word_chars=PARAGRAPH_MIN_WORD_CHARS; state.start_offset=offset;
            if let Some(detection)=complete_paragraph(&mut state) { assert_eq!(detection.detail["paragraphChars"],DetailValue::Number(length as f64)); return; }
        }
        panic!("third matching paragraph must be detected");
    }
    fn feed(text: &str) -> Option<DetectorMatch> { let mut state = create_paragraph_repeat_state(); ScalarScanner::default().push(&text.encode_utf16().collect::<Vec<_>>()).iter().find_map(|entry| update_paragraph_repeats(&mut state, entry)) }
    #[test] fn third_paragraph_repeat_points_to_first_and_second() { let paragraph = "the same long paragraph describing our work and observations in enough detail to exceed the threshold"; let input = format!("{paragraph}\n\n").repeat(3); let result = feed(&input).unwrap(); assert_eq!(result.anomaly_start_offset, 0); assert_eq!(result.garbage_start_offset, paragraph.len() + 2); }
    #[test] fn nonconsecutive_repetitions_are_counted() { let paragraph = "the same long paragraph describing our work and observations in enough detail to exceed the threshold"; let input = format!("{paragraph}\n\nother\n\n{paragraph}\n\ndifferent\n\n{paragraph}\n\n"); let result = feed(&input); assert!(result.is_some()); }
    #[test] fn two_paragraphs_are_below_threshold() { let paragraph = "the same long paragraph describing our work and observations in enough detail to exceed the threshold"; let input = format!("{paragraph}\n\n").repeat(2); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn short_paragraphs_are_exempt() { let input = "short\n\n".repeat(20); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn paragraphs_without_enough_word_characters_are_exempt() { let input = format!("{}\n\n", "!".repeat(100)).repeat(4); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn no_blank_separator_leaves_paragraph_open() { let input = "long content with words to explain the task and the result again and again\n".repeat(4); let result = feed(&input); assert!(result.is_none()); }
}
