use std::{collections::BTreeSet, sync::OnceLock};
use regex::Regex;
use crate::types::{DetectorMatch, DetectorRule, DetailValue};
pub const REPETITIVE_TURNS_RULE_NAME: &str = "repetitive-turns";
pub const REPETITIVE_TURNS_SIMILARITY_THRESHOLD: f64 = 0.55;
pub const REPETITIVE_TURNS_STREAK_LENGTH: u32 = 3;
pub const REPETITIVE_TURNS_MIN_NORMALIZED_CHARS: usize = 40;
pub const REPETITIVE_TURNS_HISTORY_CAPACITY: usize = 6;
fn expression(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex { cell.get_or_init(|| match Regex::new(pattern) { Ok(regex) => regex, Err(error) => panic!("invalid static repetitive-turn expression: {error}") }) }
fn whitespace(c: char) -> bool { matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}') }
pub fn normalize_turn_text(text: &str) -> String {
    static HEX: OnceLock<Regex> = OnceLock::new(); static DIGITS: OnceLock<Regex> = OnceLock::new();
    let lowered = text.to_lowercase();
    let folded = expression(&HEX, r"(?i)(?-u:\b)[0-9a-f]{7,}(?-u:\b)").replace_all(&lowered, "#");
    let folded = expression(&DIGITS, r"[0-9][0-9.,:/-]*").replace_all(&folded, "#");
    folded.split(whitespace).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}
fn word_trigrams(normalized: &str) -> BTreeSet<String> {
    static WORD: OnceLock<Regex> = OnceLock::new();
    let words: Vec<_> = expression(&WORD, r"[\p{L}\p{N}#]+").find_iter(normalized).map(|m| m.as_str()).collect();
    words.windows(3).map(|words| words.join(" ")).collect()
}
fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    let intersection = a.intersection(b).count();
    f64::from(u32::try_from(intersection).unwrap_or(u32::MAX)) / f64::from(u32::try_from(a.len() + b.len() - intersection).unwrap_or(u32::MAX))
}
pub fn trigram_jaccard(a: &str, b: &str) -> f64 { jaccard(&word_trigrams(a), &word_trigrams(b)) }
pub fn is_near_duplicate_of_previous_turn(candidate: &str, previous: &str) -> bool { trigram_jaccard(candidate, previous) >= REPETITIVE_TURNS_SIMILARITY_THRESHOLD }
pub struct TurnEntry { pub normalized: String, pub grams: BTreeSet<String> }
#[derive(Default)]
pub struct RepetitiveTurnsState { pub history: Vec<TurnEntry>, pub streak: u32, pub latched: bool }
pub fn create_repetitive_turns_state() -> RepetitiveTurnsState { RepetitiveTurnsState::default() }
pub fn record_turn_text(state: &mut RepetitiveTurnsState, text: &str) -> Option<DetectorMatch> {
    let normalized = normalize_turn_text(text);
    if normalized.encode_utf16().count() < REPETITIVE_TURNS_MIN_NORMALIZED_CHARS { return None; }
    let grams = word_trigrams(&normalized);
    let similarity = state.history.last().map_or(0.0, |previous| jaccard(&grams, &previous.grams));
    let sample = normalized.chars().scan(0_usize, |width, c| { *width += c.len_utf16(); (*width <= 80).then_some(c) }).collect();
    state.history.push(TurnEntry { normalized, grams });
    if state.history.len() > REPETITIVE_TURNS_HISTORY_CAPACITY { state.history.remove(0); }
    if similarity >= REPETITIVE_TURNS_SIMILARITY_THRESHOLD { state.streak += 1; } else { state.streak = 0; state.latched = false; }
    if state.latched || state.streak < REPETITIVE_TURNS_STREAK_LENGTH - 1 { return None; }
    state.latched = true;
    Some(DetectorMatch { rule: DetectorRule::RepetitiveTurns, reason: format!("assistant repeated a near-identical message across {} consecutive turns (jaccard {similarity:.2})", state.streak + 1), anomaly_start_offset: 0, garbage_start_offset: 0, detail: [("mechanism".into(), DetailValue::String("cross-turn".into())), ("streak".into(), DetailValue::Number(f64::from(state.streak + 1))), ("similarity".into(), DetailValue::Number((similarity * 1000.0).round() / 1000.0)), ("sample".into(), DetailValue::String(sample))].into() })
}
#[cfg(test)] mod tests {
    use super::*;
    const STATUS: &str = "I read this as continue supervising the portable-PTY matrix; it has started cleanly with 1 check green and 8 pending.";
    #[test] fn normalizes_digits_timestamps_and_hex_ids() { let result = [normalize_turn_text("3 checks Green, 6 remain at 06:08:27"), normalize_turn_text("run 30882887316 on head 9a201e8 done")]; assert_eq!(result, ["# checks green, # remain at #", "run # on head # done"]); }
    #[test] fn collapses_whitespace() { let result = normalize_turn_text("a   b\tc\nd"); assert_eq!(result, "a b c d"); }
    #[test] fn identical_and_disjoint_trigrams_score_extremes() { let normalized = normalize_turn_text(STATUS); let result = (trigram_jaccard(&normalized, &normalized), trigram_jaccard("alpha beta gamma delta epsilon", "one two three four five")); assert!((result.0 - 1.0).abs() < f64::EPSILON); assert!(result.1.abs() < f64::EPSILON); }
    #[test] fn changed_suffix_can_remain_near_duplicate() { let a = normalize_turn_text("Still working on the frobnicate step. Pass 1 of 9 is done, queue drained here."); let b = normalize_turn_text("Still working on the frobnicate step. Pass 2 of 9 is done, queue drained again."); let result = trigram_jaccard(&a, &b); assert!(result >= 0.55); }
    #[test] fn normalized_status_variants_are_similar() { let a = normalize_turn_text(STATUS); let b = normalize_turn_text(&STATUS.replace("1 check", "2 checks").replace("8 pending", "7 pending")); let result = trigram_jaccard(&a, &b); assert!(result >= 0.55); }
    #[test] fn genuinely_different_updates_are_dissimilar() { let a = normalize_turn_text(STATUS); let b = normalize_turn_text("The run completed by hitting the Windows job timeout at the limit; both Windows jobs were cancelled, not assertion-failed."); let result = trigram_jaccard(&a, &b); assert!(result < 0.35); }
    #[test] fn third_near_duplicate_turn_fires() { let mut state = create_repetitive_turns_state(); let first = record_turn_text(&mut state, STATUS); let second = record_turn_text(&mut state, STATUS); let third = record_turn_text(&mut state, STATUS); assert!(first.is_none()); assert!(second.is_none()); assert_eq!(third.unwrap().rule, DetectorRule::RepetitiveTurns); }
    #[test] fn changing_updates_stay_silent() { let mut state = create_repetitive_turns_state(); let results = [STATUS, "The two reds are again Linux jobs while Windows and macOS x64 remain active. I am switching to a run-level watcher for the current head.", "Windows now passes the PTY integration binary; the next store tests hardcode a shell path. I am gating only the live lifecycle cases."].map(|text| record_turn_text(&mut state, text)); assert!(results.into_iter().all(|r| r.is_none())); }
    #[test] fn short_identical_turns_are_ignored() { let mut state = create_repetitive_turns_state(); let results = (0..3).map(|_| record_turn_text(&mut state, "ok")).collect::<Vec<_>>(); assert!(results.into_iter().all(|r| r.is_none())); }
    #[test] fn latch_suppresses_refire_and_dissimilar_turn_resets() { let mut state = create_repetitive_turns_state(); for _ in 0..3 { record_turn_text(&mut state, STATUS); } let fourth = record_turn_text(&mut state, STATUS); let reset = record_turn_text(&mut state, "The Windows timeout occurs inside the integration binary: only the two pure buffer tests finish; every live lifecycle test stalls."); assert!(fourth.is_none()); assert!(reset.is_none()); assert!(!state.latched); assert_eq!(state.streak, 0); }
    #[test] fn old_unrelated_turn_does_not_break_current_streak() { let mut state = create_repetitive_turns_state(); record_turn_text(&mut state, "Completely unrelated planning note about refactoring the parser into smaller units with tests."); record_turn_text(&mut state, STATUS); record_turn_text(&mut state, STATUS); let result = record_turn_text(&mut state, STATUS); assert!(result.is_some()); }
}
