use crate::{stream_utils::{CharCode, FixedRing, ScalarEntry, is_ascii_whitespace}, types::{DetectorMatch, DetectorRule, DetailValue}};
use super::collapse_scalars::is_decorative_scalar;
pub const LINE_RING_CAPACITY: usize = 16;
pub const LINE_PERIOD_MAX: usize = 4;
pub const LINE_CYCLE_MIN_CYCLES: u32 = 6;
pub const LINE_MIN_REPEATED_CHARS: usize = 384;
pub const LINE_TEXT_RETENTION_MAX: usize = 512;
const HASH_A_OFFSET: u32 = 0x811c9dc5;
const HASH_B_OFFSET: u32 = 5381;
struct LineEntry { hash_a: u32, hash_b: u32, utf16_length: usize, start_offset: usize, eligible: bool, text: Option<Vec<u16>> }
pub struct LineCycleState {
    ring: FixedRing<LineEntry>, matched: [u32; LINE_PERIOD_MAX + 1], repeated_chars: [usize; LINE_PERIOD_MAX + 1], block_start: [usize; LINE_PERIOD_MAX + 1], cycle_width: [usize; LINE_PERIOD_MAX + 1],
    hash_a: u32, hash_b: u32, length: usize, start_offset: usize, has_content: bool, retained: Vec<u16>, retaining: bool,
}
pub fn create_line_cycle_state() -> LineCycleState { LineCycleState { ring: FixedRing::new(LINE_RING_CAPACITY), matched: [0; LINE_PERIOD_MAX + 1], repeated_chars: [0; LINE_PERIOD_MAX + 1], block_start: [0; LINE_PERIOD_MAX + 1], cycle_width: [0; LINE_PERIOD_MAX + 1], hash_a: HASH_A_OFFSET, hash_b: HASH_B_OFFSET, length: 0, start_offset: 0, has_content: false, retained: Vec::new(), retaining: true } }
pub fn update_line_cycles(state: &mut LineCycleState, entry: &ScalarEntry) -> Option<DetectorMatch> {
    if entry.value.first() == Some(&CharCode::LINE_FEED) { return complete_line(state, entry); }
    for code in &entry.value { state.hash_a = (state.hash_a ^ u32::from(*code)).wrapping_mul(0x01000193); state.hash_b = state.hash_b.wrapping_mul(31).wrapping_add(u32::from(*code)); }
    let code = match entry.value.as_slice() { [high, low, ..] if (0xd800..=0xdbff).contains(high) && (0xdc00..=0xdfff).contains(low) => 0x10000 + ((u32::from(*high) - 0xd800) << 10) + u32::from(*low) - 0xdc00, [code, ..] => u32::from(*code), [] => 0 };
    if !state.has_content && !u16::try_from(code).is_ok_and(is_ascii_whitespace) && !is_decorative_scalar(code) { state.has_content = true; }
    state.length += entry.width;
    if state.retaining { state.retained.extend_from_slice(&entry.value); if state.retained.len() > LINE_TEXT_RETENTION_MAX { state.retaining = false; state.retained.clear(); } }
    None
}
fn complete_line(state: &mut LineCycleState, entry: &ScalarEntry) -> Option<DetectorMatch> {
    let line = LineEntry { hash_a: state.hash_a, hash_b: state.hash_b, utf16_length: state.length, start_offset: state.start_offset, eligible: state.has_content, text: state.retaining.then(|| state.retained.clone()) };
    let hash_a = line.hash_a; let hash_b = line.hash_b; let length = line.utf16_length; let eligible = line.eligible;
    let sample = line.text.as_ref().map(|text| String::from_utf16_lossy(&text[..text.len().min(80)])).unwrap_or_default();
    state.ring.push(line);
    state.hash_a = HASH_A_OFFSET; state.hash_b = HASH_B_OFFSET; state.length = 0; state.start_offset = entry.start_offset + 1; state.has_content = false; state.retained.clear(); state.retaining = true;
    for period in 1..=LINE_PERIOD_MAX {
        let Some(back) = state.ring.get_back(period).filter(|back| back.eligible && eligible && back.utf16_length == length && back.hash_a == hash_a && back.hash_b == hash_b) else { state.matched[period] = 0; state.repeated_chars[period] = 0; continue; };
        if state.matched[period] == 0 {
            state.block_start[period] = back.start_offset;
            state.cycle_width[period] = (1..=period).filter_map(|back| state.ring.get_back(back)).map(|line| line.utf16_length + 1).sum();
            state.repeated_chars[period] = (0..=period).filter_map(|back| state.ring.get_back(back)).map(|line| line.utf16_length).sum();
            state.matched[period] = 1;
        } else { state.matched[period] += 1; state.repeated_chars[period] += length; }
        let period_number = u32::try_from(period).unwrap_or(u32::MAX);
        if state.matched[period] < (LINE_CYCLE_MIN_CYCLES - 1) * period_number || state.repeated_chars[period] < LINE_MIN_REPEATED_CHARS { continue; }
        let cycles = state.matched[period] / period_number + 1;
        return Some(DetectorMatch { rule: DetectorRule::CollapseRepetition, reason: format!("line cycle with period {period} over {cycles} cycles ({} repeated chars)", state.repeated_chars[period]), anomaly_start_offset: state.block_start[period], garbage_start_offset: state.block_start[period] + state.cycle_width[period], detail: [("mechanism".into(), DetailValue::String("line-cycle".into())), ("period".into(), DetailValue::Number(f64::from(period_number))), ("cycles".into(), DetailValue::Number(f64::from(cycles))), ("repeatedChars".into(), DetailValue::Number(state.repeated_chars[period] as f64)), ("sample".into(), DetailValue::String(sample))].into() });
    }
    None
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::stream_utils::ScalarScanner;
    #[test] fn repeated_character_detail_preserves_lengths_above_u32() {
        let mut state=create_line_cycle_state();
        state.ring.push(LineEntry { hash_a:HASH_A_OFFSET,hash_b:HASH_B_OFFSET,utf16_length:0,start_offset:0,eligible:true,text:None });
        state.has_content=true; state.matched[1]=4; state.repeated_chars[1]=u32::MAX as usize+100;
        let newline=ScalarScanner::default().push(&[CharCode::LINE_FEED]).remove(0);
        let detection=update_line_cycles(&mut state,&newline).unwrap(); assert_eq!(detection.detail["repeatedChars"],DetailValue::Number((u32::MAX as usize+100) as f64));
    }
    fn feed(text: &str) -> Option<DetectorMatch> { let mut state = create_line_cycle_state(); ScalarScanner::default().push(&text.encode_utf16().collect::<Vec<_>>()).iter().find_map(|entry| update_line_cycles(&mut state, entry)) }
    #[test] fn six_long_lines_fire_single_line_cycle() { let line = format!("hello {}", "x".repeat(70)); let input = format!("{line}\n").repeat(6); let result = feed(&input).unwrap(); assert_eq!(result.detail["period"], DetailValue::Number(1.0)); assert_eq!(result.garbage_start_offset, line.len() + 1); }
    #[test] fn alternating_long_lines_fire_period_two() { let a = format!("first {}", "x".repeat(70)); let b = format!("second {}", "y".repeat(70)); let input = format!("{a}\n{b}\n").repeat(6); let result = feed(&input).unwrap(); assert_eq!(result.detail["period"], DetailValue::Number(2.0)); assert_eq!(result.garbage_start_offset, a.len() + b.len() + 2); }
    #[test] fn five_lines_stay_below_cycle_threshold() { let input = format!("hello {}\n", "x".repeat(70)).repeat(5); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn short_lines_stay_below_character_threshold() { let input = "hello\n".repeat(6); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn decorative_lines_are_exempt() { let input = format!("{}\n", "=".repeat(100)).repeat(10); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn hash_mismatch_breaks_recurrence() { let input = (0..10).map(|n| format!("{n} {}\n", "x".repeat(100))).collect::<String>(); let result = feed(&input); assert!(result.is_none()); }
    #[test] fn retained_sample_is_bounded() { let input = format!("{}\n", "x".repeat(600)).repeat(6); let result = feed(&input).unwrap(); assert_eq!(result.detail["sample"], DetailValue::String(String::new())); }
}
