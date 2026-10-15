use std::collections::BTreeSet;
use serde_json::Value;
use crate::detectors::repetitive_turns::{create_repetitive_turns_state, jaccard_counts, normalize_turn_text, record_turn_text, turn_trigrams, turn_words, RepetitiveTurnsState, REPETITIVE_TURNS_MIN_NORMALIZED_CHARS, REPETITIVE_TURNS_RULE_NAME, REPETITIVE_TURNS_SIMILARITY_THRESHOLD};
pub fn collect_assistant_text(content: &Value) -> Option<String> { let text=content.as_array()?.iter().filter(|block| block.get("type").and_then(Value::as_str)==Some("text")).filter_map(|block| block.get("text").and_then(Value::as_str)).collect::<String>(); (!text.is_empty()).then_some(text) }
pub fn read_persisted_assistant_texts(entries: &[Value]) -> Vec<String> { let mut texts: Vec<_>=entries.iter().filter(|entry| entry.get("type").and_then(Value::as_str)==Some("message")).filter_map(|entry| entry.get("message")).filter(|message| message.get("role").and_then(Value::as_str)==Some("assistant")).filter_map(|message| message.get("content").and_then(collect_assistant_text)).collect(); if texts.len()>8 { texts.drain(..texts.len()-8); } texts }
/// Normalized trigram state for the current turn's text up to its last ASCII whitespace.
/// Normalization never crosses ASCII whitespace (case folding context, hex/digit folds and
/// word splitting all stop there), so the committed prefix never needs recomputation.
#[derive(Default)]
struct CommittedTurnPrefix { raw_len: usize, normalized_utf16: usize, last_words: Vec<String>, grams: BTreeSet<String>, intersection: usize }
impl CommittedTurnPrefix {
    fn push_word(&mut self, word: &str, previous: &BTreeSet<String>) {
        if self.last_words.len() == 2 { let gram = format!("{} {} {word}", self.last_words[0], self.last_words[1]); if previous.contains(&gram) && !self.grams.contains(&gram) { self.intersection += 1; } self.grams.insert(gram); self.last_words.remove(0); }
        self.last_words.push(word.to_owned());
    }
    fn advance(&mut self, text: &str, previous: &BTreeSet<String>) {
        let Some(end) = text[self.raw_len..].rfind(|c: char| c.is_ascii_whitespace()).map(|index| self.raw_len + index + 1) else { return; };
        let normalized = normalize_turn_text(&text[self.raw_len..end]);
        self.raw_len = end;
        if normalized.is_empty() { return; }
        self.normalized_utf16 += usize::from(self.normalized_utf16 > 0) + normalized.encode_utf16().count();
        for word in turn_words(&normalized) { self.push_word(word, previous); }
    }
    /// Same verdict as normalizing the whole text and comparing its trigram set with `previous`.
    fn is_near_duplicate(&mut self, text: &str, previous: &BTreeSet<String>) -> bool {
        self.advance(text, previous);
        let tail = normalize_turn_text(&text[self.raw_len..]);
        let tail_utf16 = tail.encode_utf16().count();
        let separator = usize::from(self.normalized_utf16 > 0 && tail_utf16 > 0);
        if self.normalized_utf16 + separator + tail_utf16 < REPETITIVE_TURNS_MIN_NORMALIZED_CHARS { return false; }
        let mut window: Vec<&str> = self.last_words.iter().map(String::as_str).collect();
        let mut extra = BTreeSet::new();
        for word in turn_words(&tail) { if window.len() == 2 { let gram = format!("{} {} {word}", window[0], window[1]); if !self.grams.contains(&gram) { extra.insert(gram); } window.remove(0); } window.push(word); }
        let intersection = self.intersection + extra.iter().filter(|gram| previous.contains(*gram)).count();
        jaccard_counts(self.grams.len() + extra.len(), previous.len(), intersection) >= REPETITIVE_TURNS_SIMILARITY_THRESHOLD
    }
}
pub struct RepetitiveTurnsLane { state: RepetitiveTurnsState, armed: bool, recovering: bool, current_turn_text: String, committed: CommittedTurnPrefix, last_completed_turn_grams: Option<BTreeSet<String>>, enabled: bool }
impl Default for RepetitiveTurnsLane { fn default() -> Self { Self { state: create_repetitive_turns_state(), armed:false, recovering:false, current_turn_text:String::new(), committed:CommittedTurnPrefix::default(), last_completed_turn_grams:None, enabled:true } } }
impl RepetitiveTurnsLane {
    pub fn configure(&mut self, disabled_rules: &BTreeSet<String>) { self.enabled = !disabled_rules.contains(REPETITIVE_TURNS_RULE_NAME); }
    pub fn reset_session(&mut self) { self.state=create_repetitive_turns_state(); self.armed=false; self.recovering=false; self.current_turn_text.clear(); self.committed=CommittedTurnPrefix::default(); self.last_completed_turn_grams=None; }
    pub fn restore_from_history(&mut self, texts: &[String]) { for text in texts { self.record_completed_turn(text); } self.armed=false; }
    pub fn reset_turn(&mut self) { self.armed=false; self.current_turn_text.clear(); self.committed=CommittedTurnPrefix::default(); }
    pub fn disarm(&mut self) { self.armed=false; }
    pub const fn armed(&self) -> bool { self.armed }
    pub fn observe_text_delta(&mut self, delta: &str, can_arm: bool) -> bool {
        self.current_turn_text.push_str(delta);
        if self.enabled && !self.armed && !self.recovering && can_arm && let Some(previous)=&self.last_completed_turn_grams && self.committed.is_near_duplicate(&self.current_turn_text,previous) { self.armed=true; }
        self.armed
    }
    pub fn commit_armed_turn(&mut self, text: Option<&str>) { self.armed=false; self.recovering=true; if let Some(text)=text { self.last_completed_turn_grams=Some(turn_trigrams(&normalize_turn_text(text))); self.committed=CommittedTurnPrefix::default(); } }
    pub fn record_completed_turn(&mut self, text: &str) {
        self.last_completed_turn_grams=Some(turn_trigrams(&normalize_turn_text(text))); self.committed=CommittedTurnPrefix::default();
        if !self.enabled { return; }
        if record_turn_text(&mut self.state,text).is_none() { self.recovering=false; } else { self.armed=true; }
    }
}
#[cfg(test)] mod tests {
    use super::*;
    const TEXT: &str="The current matrix is progressing through the native integration checks while the remaining jobs continue running.";
    #[test] fn previous_turn_arms_matching_stream() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); lane.reset_turn(); let result=lane.observe_text_delta(TEXT,true); assert!(result); }
    #[test] fn disabled_rule_does_not_arm() { let mut lane=RepetitiveTurnsLane::default(); lane.configure(&BTreeSet::from([REPETITIVE_TURNS_RULE_NAME.into()])); lane.record_completed_turn(TEXT); let result=lane.observe_text_delta(TEXT,true); assert!(!result); }
    #[test] fn recovery_suppresses_stream_arm() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); lane.commit_armed_turn(Some(TEXT)); let result=lane.observe_text_delta(TEXT,true); assert!(!result); }
    #[test] fn restored_history_does_not_leave_lane_armed() { let mut lane=RepetitiveTurnsLane::default(); lane.restore_from_history(&vec![TEXT.into();3]); let result=lane.armed(); assert!(!result); }
    #[test] fn permission_to_arm_is_required() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); let result=lane.observe_text_delta(TEXT,false); assert!(!result); }
    #[test] fn reset_session_forgets_prior_turn() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); lane.reset_session(); let result=lane.observe_text_delta(TEXT,true); assert!(!result); }
    #[test] fn persisted_history_retains_last_eight_assistant_texts() { let entries=(0..10).map(|i| serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":i.to_string()}]}})).collect::<Vec<_>>(); let result=read_persisted_assistant_texts(&entries); assert_eq!(result.len(),8); assert_eq!(result[0],"2"); assert_eq!(result[7],"9"); }
    #[test] fn incremental_verdict_matches_full_recompute_over_random_deltas() {
        use crate::detectors::repetitive_turns::is_near_duplicate_of_previous_grams;
        const PIECES: &[&str]=&["the ","matrix ","is ","Progressing ","through ","checks ","while ","jobs ","run","ning ","9a201e8 ","abcdef0123","3 ","06:08:27","ΟΔΟΣ ","ΑΣ","\u{feff}","Β ","\u{a0}","\t","\n","  ","x","y ",", ",". ","#","İ","ß ","中文 ","e\u{301} ",""];
        let mut seed=0x9e37_79b9_7f4a_7c15_u64; let mut next=|bound: usize| { seed^=seed<<13; seed^=seed>>7; seed^=seed<<17; usize::try_from(seed%bound as u64).unwrap() };
        let (mut hits,mut misses)=(0,0);
        for trial in 0..300 {
            let pieces: Vec<&str>=(0..30).map(|_| PIECES[next(PIECES.len())]).collect();
            let previous_text: String=pieces.concat();
            let previous=turn_trigrams(&normalize_turn_text(&previous_text));
            let stream: Vec<char>=if trial%2==0 { pieces.iter().map(|piece| if next(6)==0 { PIECES[next(PIECES.len())] } else { piece }).collect::<String>().chars().collect() } else { (0..40).map(|_| PIECES[next(PIECES.len())]).collect::<String>().chars().collect() };
            let mut prefix=CommittedTurnPrefix::default(); let mut text=String::new(); let mut at=0;
            while at<stream.len() {
                let end=(at+1+next(8)).min(stream.len()); text.extend(&stream[at..end]); at=end;
                if next(4)==0 && at<stream.len() { continue; }
                let full=normalize_turn_text(&text); let expected=full.encode_utf16().count()>=REPETITIVE_TURNS_MIN_NORMALIZED_CHARS && is_near_duplicate_of_previous_grams(&full,&previous);
                assert_eq!(prefix.is_near_duplicate(&text,&previous),expected,"text={text:?} previous={previous_text:?}");
                if expected { hits+=1; } else { misses+=1; }
            }
        }
        assert!(hits>0 && misses>0,"hits={hits} misses={misses}");
    }
    #[test] fn text_collection_concatenates_without_separator() { let content=serde_json::json!([{"type":"text","text":"one"},{"type":"thinking","thinking":"ignore"},{"type":"text","text":"two"}]); let result=collect_assistant_text(&content); assert_eq!(result.as_deref(),Some("onetwo")); }
}
