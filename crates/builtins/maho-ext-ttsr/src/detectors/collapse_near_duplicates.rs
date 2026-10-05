use std::{collections::BTreeSet, sync::OnceLock};
use regex::Regex;
use crate::{stream_utils::{CharCode, FixedRing, ScalarEntry, is_ascii_whitespace}, types::{DetectorMatch, DetectorRule, DetailValue}};
use super::{collapse_scalars::{is_ascii_alphanumeric, is_box_drawing}, repetitive_turns::normalize_turn_text};
pub const NEAR_DUPLICATE_MIN_CHARS: usize = 64;
pub const NEAR_DUPLICATE_MIN_WORD_CHARS: u32 = 24;
pub const NEAR_DUPLICATE_SIMILARITY: f64 = 0.5;
pub const NEAR_DUPLICATE_LOOKBACK: usize = 32;
pub const NEAR_DUPLICATE_WINDOW: usize = 12;
pub const NEAR_DUPLICATE_ECHO_THRESHOLD: usize = 8;
pub const NEAR_DUPLICATE_TEXT_RETENTION_MAX: usize = 512;
struct ParagraphSignature { tokens: BTreeSet<String>, start_offset: usize, sample: String }
struct WindowEntry { echoed: bool, start_offset: usize, anchor_start_offset: usize }
pub struct NearDuplicateState { history: FixedRing<ParagraphSignature>, window: FixedRing<WindowEntry>, line_length: usize, line_word_chars: u32, line_has_content: bool, line_start_offset: usize, line_text: Vec<u16>, length: usize, word_chars: u32, start_offset: usize, retained: Vec<u16>, fenced: bool, inside_fence: bool }
pub fn create_near_duplicate_state() -> NearDuplicateState { NearDuplicateState { history: FixedRing::new(NEAR_DUPLICATE_LOOKBACK), window: FixedRing::new(NEAR_DUPLICATE_WINDOW), line_length: 0, line_word_chars: 0, line_has_content: false, line_start_offset: 0, line_text: Vec::new(), length: 0, word_chars: 0, start_offset: 0, retained: Vec::new(), fenced: false, inside_fence: false } }
fn complete_paragraph(state: &mut NearDuplicateState) -> Option<DetectorMatch> {
    let eligible = !state.fenced && state.length >= NEAR_DUPLICATE_MIN_CHARS && state.word_chars >= NEAR_DUPLICATE_MIN_WORD_CHARS;
    let text = std::mem::take(&mut state.retained); let start_offset = state.start_offset;
    state.length = 0; state.word_chars = 0; state.start_offset = 0; state.fenced = false;
    if !eligible { return None; }
    static WORDS: OnceLock<Regex> = OnceLock::new();
    let pattern = WORDS.get_or_init(|| match Regex::new(r"[\p{L}\p{N}#]+") { Ok(regex) => regex, Err(error) => panic!("invalid static word pattern: {error}") });
    let normalized = normalize_turn_text(&String::from_utf16_lossy(&text));
    let tokens: BTreeSet<String> = pattern.find_iter(&normalized).map(|m| m.as_str().to_owned()).collect();
    if tokens.is_empty() { return None; }
    let mut best = 0.0; let mut anchor = None;
    for back in 0..state.history.size() {
        let Some(previous) = state.history.get_back(back) else { continue; };
        if previous.tokens.is_empty() { continue; }
        let intersection = tokens.intersection(&previous.tokens).count();
        let similarity = f64::from(u32::try_from(intersection).unwrap_or(u32::MAX)) / f64::from(u32::try_from(tokens.len() + previous.tokens.len() - intersection).unwrap_or(u32::MAX));
        if similarity > best { best = similarity; anchor = Some((previous.start_offset, previous.sample.clone())); }
    }
    let echoed = best >= NEAR_DUPLICATE_SIMILARITY && anchor.is_some();
    state.history.push(ParagraphSignature { tokens, start_offset, sample: String::from_utf16_lossy(&text[..text.len().min(80)]) });
    state.window.push(WindowEntry { echoed, start_offset, anchor_start_offset: if echoed { anchor.as_ref().map_or(start_offset, |a| a.0) } else { start_offset } });
    let echoes = (0..state.window.size()).filter(|back| state.window.get_back(*back).is_some_and(|e| e.echoed)).count();
    if echoes < NEAR_DUPLICATE_ECHO_THRESHOLD { return None; }
    let first = (0..state.window.size()).rev().find_map(|back| state.window.get_back(back).filter(|e| e.echoed))?;
    Some(DetectorMatch { rule: DetectorRule::CollapseRepetition, reason: format!("{echoes} of the last {} paragraphs restate an earlier paragraph of the same message", state.window.size()), anomaly_start_offset: first.anchor_start_offset, garbage_start_offset: first.start_offset, detail: [("mechanism".into(), DetailValue::String("near-duplicate-paragraphs".into())), ("echoes".into(), DetailValue::Number(f64::from(u32::try_from(echoes).unwrap_or(u32::MAX)))), ("window".into(), DetailValue::Number(f64::from(u32::try_from(state.window.size()).unwrap_or(u32::MAX)))), ("similarity".into(), DetailValue::Number((best * 1000.0).round() / 1000.0)), ("sample".into(), DetailValue::String(anchor.map_or_else(String::new, |a| a.1)))].into() })
}
pub fn update_near_duplicates(state: &mut NearDuplicateState, entry: &ScalarEntry) -> Option<DetectorMatch> {
    if entry.value.first() == Some(&CharCode::LINE_FEED) {
        let result = if state.line_has_content {
            if state.length == 0 { state.start_offset = state.line_start_offset; }
            state.length += state.line_length + 1; state.word_chars += state.line_word_chars;
            let line = String::from_utf16_lossy(&state.line_text);
            let trimmed = line.trim_start_matches(|c| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
            let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
            if state.inside_fence || fence { state.fenced = true; }
            if state.retained.len() < NEAR_DUPLICATE_TEXT_RETENTION_MAX { state.retained.extend_from_slice(&state.line_text); state.retained.push(CharCode::LINE_FEED); }
            if fence { state.inside_fence = !state.inside_fence; }
            None
        } else if state.length > 0 { complete_paragraph(state) } else { None };
        state.line_length = 0; state.line_word_chars = 0; state.line_has_content = false; state.line_start_offset = entry.start_offset + 1; state.line_text.clear(); return result;
    }
    let code = match entry.value.as_slice() { [high, low, ..] if (0xd800..=0xdbff).contains(high) && (0xdc00..=0xdfff).contains(low) => 0x10000 + ((u32::from(*high) - 0xd800) << 10) + u32::from(*low) - 0xdc00, [code, ..] => u32::from(*code), [] => 0 };
    if !u16::try_from(code).is_ok_and(is_ascii_whitespace) { state.line_has_content = true; }
    if is_ascii_alphanumeric(code) || code > 0x7f && !is_box_drawing(code) { state.line_word_chars += 1; }
    state.line_length += entry.width;
    if state.line_text.len() < NEAR_DUPLICATE_TEXT_RETENTION_MAX { state.line_text.extend_from_slice(&entry.value); }
    None
}
#[cfg(test)] mod tests {
    use super::*; use crate::stream_utils::ScalarScanner;
    const PARAGRAPH: &str = "The current matrix is progressing through the native integration checks while the remaining jobs continue running.";
    fn feed(input: &str) -> Option<DetectorMatch> { let mut state = create_near_duplicate_state(); ScalarScanner::default().push(&input.encode_utf16().collect::<Vec<_>>()).iter().find_map(|e| update_near_duplicates(&mut state, e)) }
    #[test] fn eight_echoes_trigger_at_ninth_paragraph() { let input = format!("{PARAGRAPH}\n\n").repeat(9); let result = feed(&input).unwrap(); assert_eq!(result.anomaly_start_offset, 0); assert_eq!(result.garbage_start_offset, PARAGRAPH.len() + 2); }
    #[test] fn seven_echoes_stay_below_threshold() { let result = feed(&format!("{PARAGRAPH}\n\n").repeat(8)); assert!(result.is_none()); }
    #[test] fn digit_changes_are_normalized() { let input = (0..9).map(|i| format!("{PARAGRAPH} Pass {i} is pending.\n\n")).collect::<String>(); let result = feed(&input); assert!(result.is_some()); }
    #[test] fn fenced_paragraphs_are_exempt() { let input = format!("```\n{PARAGRAPH}\n```\n\n").repeat(12); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn fence_state_survives_blank_lines() { let input = format!("```\n\n{}\n```\n\n", format!("{PARAGRAPH}\n\n").repeat(12)); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn short_paragraphs_are_exempt() { let result = feed(&"short\n\n".repeat(20)); assert!(result.is_none()); }
    #[test] fn no_blank_separator_leaves_paragraph_open() { let result = feed(&format!("{PARAGRAPH}\n").repeat(20)); assert!(result.is_none()); }
}
