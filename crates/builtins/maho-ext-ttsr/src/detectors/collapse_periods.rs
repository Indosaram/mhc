use crate::{stream_utils::{FixedRing, ScalarEntry, is_ascii_whitespace}, types::{DetectorMatch, DetectorRule, DetailValue}};
use super::collapse_scalars::{is_ascii_alphanumeric, is_decorative_scalar};
pub const PERIOD_MIN: usize = 2;
pub const PERIOD_MAX: usize = 64;
pub const PERIOD_SPAN_MIN: u32 = 256;
pub const PERIOD_REPS_MIN: usize = 8;
fn eligible(unit: &[u16]) -> bool {
    !unit.iter().all(|c| is_ascii_whitespace(*c))
        && !unit.iter().all(|c| is_ascii_whitespace(*c) || is_decorative_scalar(u32::from(*c)))
        && !unit.iter().all(|c| is_ascii_alphanumeric(u32::from(*c)) || matches!(*c, 43 | 47 | 61))
        && !unit.iter().all(|c| is_ascii_whitespace(*c) || matches!(*c, 46 | 44 | 58 | 59 | 45 | 47 | 124) || (48..=57).contains(c) || (65..=70).contains(c) || (97..=102).contains(c))
        && !unit.iter().all(|c| is_ascii_whitespace(*c) || "{}()[]<>,;.:=+-*/&|^%$#@~`'\"\\_".encode_utf16().any(|punct| punct == *c))
}
pub struct ShortPeriodState { pub matched: [u32; PERIOD_MAX + 1], pub block_start: [usize; PERIOD_MAX + 1], pub unit_checked: [bool; PERIOD_MAX + 1] }
pub fn create_short_period_state() -> ShortPeriodState { ShortPeriodState { matched: [0; PERIOD_MAX + 1], block_start: [0; PERIOD_MAX + 1], unit_checked: [false; PERIOD_MAX + 1] } }
fn read_unit(ring: &FixedRing<ScalarEntry>, period: usize) -> (Vec<u16>, usize) {
    let mut text = Vec::new();
    let mut width = 0;
    for back in (0..period).rev() {
        let Some(scalar) = ring.get_back(back) else { break; };
        let mut next = scalar.value.clone(); next.extend_from_slice(&text); text = next;
        width += scalar.width;
    }
    (text, width)
}
pub fn update_short_periods(state: &mut ShortPeriodState, entry: &ScalarEntry, ring: &FixedRing<ScalarEntry>) -> Option<DetectorMatch> {
    for period in PERIOD_MIN..=PERIOD_MAX {
        let Some(back) = ring.get_back(period).filter(|back| back.value == entry.value) else { state.matched[period] = 0; state.unit_checked[period] = false; continue; };
        if state.matched[period] == 0 { state.block_start[period] = back.start_offset; }
        state.matched[period] += 1;
        let matched = state.matched[period];
        if state.unit_checked[period] || matched < PERIOD_SPAN_MIN || usize::try_from(matched).unwrap_or(usize::MAX) < PERIOD_REPS_MIN * period { continue; }
        state.unit_checked[period] = true;
        let (unit, width) = read_unit(ring, period);
        if !eligible(&unit) { continue; }
        let period_number = u32::try_from(period).unwrap_or(u32::MAX);
        return Some(DetectorMatch { rule: DetectorRule::CollapseRepetition, reason: format!("short-period recurrence with period {period} spanning {matched} scalars"), anomaly_start_offset: state.block_start[period], garbage_start_offset: state.block_start[period] + width, detail: [("mechanism".into(), DetailValue::String("short-period".into())), ("period".into(), DetailValue::Number(f64::from(period_number))), ("span".into(), DetailValue::Number(f64::from(matched))), ("repetitions".into(), DetailValue::Number(f64::from(matched / period_number + 1)))].into() });
    }
    None
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::stream_utils::ScalarScanner;
    fn feed(text: &str) -> Option<DetectorMatch> { let mut state = create_short_period_state(); let mut ring = FixedRing::new(512); for entry in ScalarScanner::default().push(&text.encode_utf16().collect::<Vec<_>>()) { ring.push(entry.clone()); if let Some(matched) = update_short_periods(&mut state, &entry, &ring) { return Some(matched); } } None }
    #[test] fn repeated_words_fire_period_four() { let input = "foo ".repeat(100); let result = feed(&input).unwrap(); assert_eq!(result.detail["period"], DetailValue::Number(4.0)); assert_eq!(result.anomaly_start_offset, 0); assert_eq!(result.garbage_start_offset, 4); }
    #[test] fn alternating_words_fire_period_seven() { let input = "yes no ".repeat(80); let result = feed(&input).unwrap(); assert_eq!(result.detail["period"], DetailValue::Number(7.0)); }
    #[test] fn punctuation_pair_is_detected() { let input = "!?".repeat(200); let result = feed(&input).unwrap(); assert_eq!(result.detail["period"], DetailValue::Number(2.0)); }
    #[test] fn short_lines_fire_period_three() { let input = "ok\n".repeat(120); let result = feed(&input).unwrap(); assert_eq!(result.detail["period"], DetailValue::Number(3.0)); }
    #[test] fn whitespace_only_is_exempt() { let input = " \n".repeat(500); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn decorative_units_are_exempt() { let input = "=-".repeat(500); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn base64_units_are_exempt() { let input = "abc/=".repeat(500); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn numeric_table_units_are_exempt() { let input = "123,abc; ".repeat(500); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn code_punctuation_is_exempt() { let input = "{}();\n".repeat(500); let result = feed(&input); assert!(result.is_none()); }
}
