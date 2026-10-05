use std::collections::VecDeque;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarEntry { pub value: Vec<u16>, pub start_offset: usize, pub width: usize }
#[derive(Default)]
pub struct ScalarScanner { offset: usize, pending_high: Option<u16> }
impl ScalarScanner {
    pub fn push(&mut self, delta: &[u16]) -> Vec<ScalarEntry> {
        let mut input = Vec::new();
        if let Some(high) = self.pending_high.take() { input.push(high); self.offset -= 1; }
        input.extend_from_slice(delta);
        let mut entries = Vec::new();
        let mut i = 0;
        while i < input.len() {
            let code = input[i];
            if (0xd800..=0xdbff).contains(&code) {
                if i + 1 >= input.len() { self.pending_high = Some(code); self.offset += 1; break; }
                if (0xdc00..=0xdfff).contains(&input[i + 1]) { entries.push(ScalarEntry { value: input[i..i + 2].to_vec(), start_offset: self.offset, width: 2 }); self.offset += 2; i += 2; continue; }
            }
            entries.push(ScalarEntry { value: vec![code], start_offset: self.offset, width: 1 }); self.offset += 1; i += 1;
        }
        entries
    }
    pub const fn has_pending_surrogate(&self) -> bool { self.pending_high.is_some() }
    pub const fn offset(&self) -> usize { self.offset }
}
pub struct FixedRing<T> { capacity: usize, entries: VecDeque<T> }
impl<T> FixedRing<T> {
    pub fn new(capacity: usize) -> Self { Self { capacity, entries: VecDeque::new() } }
    pub fn push(&mut self, entry: T) { self.entries.push_back(entry); if self.entries.len() > self.capacity { self.entries.pop_front(); } }
    pub fn get_back(&self, offset: usize) -> Option<&T> { self.entries.len().checked_sub(offset.checked_add(1)?).and_then(|index| self.entries.get(index)) }
    pub fn size(&self) -> usize { self.entries.len() }
    pub fn to_array(&self) -> Vec<T> where T: Clone { self.entries.iter().cloned().collect() }
    pub fn clear(&mut self) { self.entries.clear(); }
}
pub const fn is_ascii_whitespace(code: u16) -> bool { code == 32 || (code >= 9 && code <= 13) }
pub struct CharCode;
impl CharCode { pub const TAB: u16 = 9; pub const LINE_FEED: u16 = 10; pub const VERTICAL_TAB: u16 = 11; pub const FORM_FEED: u16 = 12; pub const CARRIAGE_RETURN: u16 = 13; pub const SPACE: u16 = 32; pub const LESS_THAN: u16 = 60; pub const LEFT_BRACKET: u16 = 91; pub const BACKSLASH: u16 = 92; pub const RIGHT_BRACKET: u16 = 93; pub const UNDERSCORE: u16 = 95; pub const PIPE: u16 = 124; }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn ascii_scalars_have_cumulative_offsets() { let mut scanner = ScalarScanner::default(); let result = scanner.push(&[97, 98, 99]); assert_eq!(result.iter().map(|e| e.start_offset).collect::<Vec<_>>(), [0, 1, 2]); assert!(result.iter().all(|e| e.width == 1)); }
    #[test] fn split_surrogates_join_across_deltas() { let mut scanner = ScalarScanner::default(); let first = scanner.push(&[120, 0xd83d]); let second = scanner.push(&[0xde00, 121]); assert_eq!(first.len(), 1); assert_eq!(second[0], ScalarEntry { value: vec![0xd83d, 0xde00], start_offset: 1, width: 2 }); assert_eq!(second[1].start_offset, 3); }
    #[test] fn isolated_high_then_low_join() { let mut scanner = ScalarScanner::default(); let first = scanner.push(&[0xd83c]); let second = scanner.push(&[0xdf0a]); assert!(first.is_empty()); assert_eq!(second[0].value, [0xd83c, 0xdf0a]); assert_eq!(second[0].start_offset, 0); }
    #[test] fn cjk_scalars_have_single_unit_width() { let mut scanner = ScalarScanner::default(); let result = scanner.push(&[0x754c, 97, 0x754c]); assert_eq!(result.iter().map(|e| e.start_offset).collect::<Vec<_>>(), [0, 1, 2]); assert!(result.iter().all(|e| e.width == 1)); }
    #[test] fn pending_surrogate_state_clears_after_pair() { let mut scanner = ScalarScanner::default(); scanner.push(&[0xd83d]); let before = scanner.has_pending_surrogate(); scanner.push(&[0xde00]); let after = scanner.has_pending_surrogate(); assert_eq!((before, after), (true, false)); }
    #[test] fn ring_keeps_latest_entries_in_order() { let mut ring = FixedRing::new(3); for n in 1..=5 { ring.push(n); } assert_eq!(ring.to_array(), [3, 4, 5]); assert_eq!(ring.size(), 3); }
    #[test] fn ring_reads_backwards_with_absent_out_of_bounds() { let mut ring = FixedRing::new(4); for s in ["a", "b", "c"] { ring.push(s); } let result = [ring.get_back(0), ring.get_back(1), ring.get_back(2), ring.get_back(3)]; assert_eq!(result, [Some(&"c"), Some(&"b"), Some(&"a"), None]); }
    #[test] fn ring_preserves_payloads_after_eviction() { let mut ring = FixedRing::new(2); for entry in [(1, 10), (2, 20), (3, 30)] { ring.push(entry); } assert_eq!(ring.get_back(0), Some(&(3, 30))); assert_eq!(ring.get_back(1), Some(&(2, 20))); }
    #[test] fn ascii_whitespace_is_accepted() { let result = [32, 9, 10, 13, 12, 11].map(is_ascii_whitespace); assert!(result.into_iter().all(|v| v)); }
    #[test] fn non_ascii_whitespace_is_rejected() { let result = [97, 48, 33, 0xa0, 0x3000].map(is_ascii_whitespace); assert!(result.into_iter().all(|v| !v)); }
}
