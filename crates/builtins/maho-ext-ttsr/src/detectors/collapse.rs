use crate::{stream_utils::{FixedRing, ScalarEntry, ScalarScanner}, types::{DetectorContext, DetectorMatch, StreamDetector, TtsrStreamSource}};
use super::{collapse_lines::{create_line_cycle_state, update_line_cycles, LineCycleState}, collapse_near_duplicates::{create_near_duplicate_state, update_near_duplicates, NearDuplicateState}, collapse_paragraphs::{create_paragraph_repeat_state, update_paragraph_repeats, ParagraphRepeatState}, collapse_periods::{create_short_period_state, update_short_periods, ShortPeriodState}, collapse_scalars::{create_dominant_run_state, create_whitespace_flood_state, update_dominant_run, update_whitespace_flood, DominantRunState, WhitespaceFloodState}};
pub const TAIL_RING_CAPACITY: usize = 512;
pub struct CollapseState { pub scanner: ScalarScanner, pub tail_ring: FixedRing<ScalarEntry>, pub run: DominantRunState, pub whitespace: WhitespaceFloodState, pub periods: ShortPeriodState, pub lines: LineCycleState, pub paragraphs: ParagraphRepeatState, pub near_duplicates: NearDuplicateState, pub latched: Option<DetectorMatch> }
pub fn create_collapse_state() -> CollapseState { CollapseState { scanner: ScalarScanner::default(), tail_ring: FixedRing::new(TAIL_RING_CAPACITY), run: create_dominant_run_state(), whitespace: create_whitespace_flood_state(), periods: create_short_period_state(), lines: create_line_cycle_state(), paragraphs: create_paragraph_repeat_state(), near_duplicates: create_near_duplicate_state(), latched: None } }
pub struct CollapseDetector;
pub const COLLAPSE_DETECTOR: CollapseDetector = CollapseDetector;
impl StreamDetector<CollapseState> for CollapseDetector {
    fn create_state(&self) -> CollapseState { create_collapse_state() }
    fn check_delta(&self, state: &mut CollapseState, delta: &[u16], context: &DetectorContext) -> Option<DetectorMatch> {
        if state.latched.is_some() { return state.latched.clone(); }
        let watch_paragraphs = context.source != TtsrStreamSource::Tool;
        for entry in state.scanner.push(delta) {
            state.tail_ring.push(entry.clone());
            let matched = update_dominant_run(&mut state.run, &entry)
                .or_else(|| update_whitespace_flood(&mut state.whitespace, &entry))
                .or_else(|| update_short_periods(&mut state.periods, &entry, &state.tail_ring))
                .or_else(|| update_line_cycles(&mut state.lines, &entry))
                .or_else(|| watch_paragraphs.then(|| update_paragraph_repeats(&mut state.paragraphs, &entry)).flatten())
                .or_else(|| watch_paragraphs.then(|| update_near_duplicates(&mut state.near_duplicates, &entry)).flatten());
            if matched.is_some() { state.latched = matched.clone(); return matched; }
        }
        None
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn context(source: TtsrStreamSource) -> DetectorContext { DetectorContext { source, stream_key: "test".into(), generation: 1 } }
    #[test] fn chunked_scalar_collapse_latches() { let mut state = create_collapse_state(); let first = COLLAPSE_DETECTOR.check_delta(&mut state, &"!".repeat(200).encode_utf16().collect::<Vec<_>>(), &context(TtsrStreamSource::Text)); let second = COLLAPSE_DETECTOR.check_delta(&mut state, &"!".repeat(100).encode_utf16().collect::<Vec<_>>(), &context(TtsrStreamSource::Text)); let third = COLLAPSE_DETECTOR.check_delta(&mut state, &"normal".encode_utf16().collect::<Vec<_>>(), &context(TtsrStreamSource::Text)); assert!(first.is_none()); assert!(second.is_some()); assert_eq!(second, third); }
    #[test] fn paragraph_repeats_are_disabled_for_tool_streams() { let paragraph = "A sufficiently long paragraph describing several observations and many concrete details about this implementation"; let text = format!("{paragraph}\n\n").repeat(3); let mut state = create_collapse_state(); let result = COLLAPSE_DETECTOR.check_delta(&mut state, &text.encode_utf16().collect::<Vec<_>>(), &context(TtsrStreamSource::Tool)); assert!(result.is_none()); }
    #[test] fn paragraph_repeats_detected_on_text_streams() { let paragraph = "A sufficiently long paragraph describing several observations and many concrete details about this implementation"; let text = format!("{paragraph}\n\n").repeat(3); let mut state = create_collapse_state(); let result = COLLAPSE_DETECTOR.check_delta(&mut state, &text.encode_utf16().collect::<Vec<_>>(), &context(TtsrStreamSource::Text)); assert!(result.is_some()); }
    #[test] fn whitespace_collapse_remains_active_on_tool_streams() { let mut state = create_collapse_state(); let result = COLLAPSE_DETECTOR.check_delta(&mut state, &" ".repeat(480).encode_utf16().collect::<Vec<_>>(), &context(TtsrStreamSource::Tool)); assert!(result.is_some()); }
}
