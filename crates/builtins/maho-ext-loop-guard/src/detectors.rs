use std::collections::{BTreeMap, BTreeSet};
use crate::{policy::*, similarity::mean_adjacent_similarity, tracker::ToolCallRecord};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoopGuardKind { Identical, Similar, Cycle }
#[derive(Clone, Debug, PartialEq)]
pub enum LoopGuardDetection {
    Identical { tool_name: String, count: usize, fingerprint: String },
    Similar { tool_name: String, count: usize, similarity: f64, fingerprint: String },
    Cycle { period: usize, count: usize, cycle_tools: Vec<String>, fingerprint: String },
}
impl LoopGuardDetection {
    pub const fn kind(&self) -> LoopGuardKind { match self { Self::Identical { .. } => LoopGuardKind::Identical, Self::Similar { .. } => LoopGuardKind::Similar, Self::Cycle { .. } => LoopGuardKind::Cycle } }
    pub fn fingerprint(&self) -> &str { match self { Self::Identical { fingerprint, .. } | Self::Similar { fingerprint, .. } | Self::Cycle { fingerprint, .. } => fingerprint } }
    pub const fn count(&self) -> usize { match self { Self::Identical { count, .. } | Self::Similar { count, .. } | Self::Cycle { count, .. } => *count } }
    fn maximum_count(&self) -> usize { match self { Self::Identical { .. } | Self::Similar { .. } => TRACK_WINDOW, Self::Cycle { period, .. } => TRACK_WINDOW / period } }
}
fn target_identity(record: &ToolCallRecord) -> Option<String> {
    let fields: &[&str] = match record.tool_name.as_str() {
        "read" => &["path"], "bash_output" => &["bash_id"], "task_output" => &["task_id", "name"], "task_update" => &["task_id"], "task_send" => &["to"], "lsp_diagnostics" => &["filePath"], _ => return None,
    };
    let args: serde_json::Value = serde_json::from_str(&record.args_json).ok()?;
    let object = args.as_object()?;
    fields.iter().find_map(|field| object.get(*field)?.as_str().filter(|s| !s.is_empty()).map(String::from))
}
pub fn detect_identical_run(records: &[ToolCallRecord]) -> Option<LoopGuardDetection> {
    let last = records.last()?;
    let count = records.iter().rev().take_while(|r| r.signature == last.signature).count();
    (count >= IDENTICAL_RUN_THRESHOLD).then(|| LoopGuardDetection::Identical { tool_name: last.tool_name.clone(), count, fingerprint: last.signature.clone() })
}
pub fn detect_similar_run(records: &[ToolCallRecord]) -> Option<LoopGuardDetection> {
    let last = records.last()?;
    let count = records.iter().rev().take_while(|r| r.tool_name == last.tool_name).count();
    if count < SIMILAR_RUN_THRESHOLD { return None; }
    let run = &records[records.len() - count..];
    let args: Vec<_> = run.iter().map(|r| r.args_json.clone()).collect();
    if args.iter().collect::<BTreeSet<_>>().len() == 1 { return None; }
    if let Some(targets) = run.iter().map(target_identity).collect::<Option<BTreeSet<_>>>()
        && targets.len() == run.len() { return None; }
    let similarity = mean_adjacent_similarity(&args);
    (similarity >= SIMILARITY_THRESHOLD).then(|| LoopGuardDetection::Similar { tool_name: last.tool_name.clone(), count, similarity, fingerprint: last.tool_name.clone() })
}
pub fn detect_cycle(records: &[ToolCallRecord]) -> Option<LoopGuardDetection> {
    let total = records.len();
    for period in CYCLE_MIN_PERIOD..=CYCLE_MAX_PERIOD {
        if total < period * CYCLE_REPETITION_THRESHOLD { continue; }
        let cycle = &records[total - period..];
        if cycle.iter().map(|r| &r.signature).collect::<BTreeSet<_>>().len() < 2 { continue; }
        let mut repetitions = 1;
        while (repetitions + 1) * period <= total {
            let start = total - (repetitions + 1) * period;
            if !records[start..start + period].iter().zip(cycle).all(|(a, b)| a.signature == b.signature) { break; }
            repetitions += 1;
        }
        if repetitions >= CYCLE_REPETITION_THRESHOLD {
            return Some(LoopGuardDetection::Cycle { period, count: repetitions, cycle_tools: cycle.iter().map(|r| r.tool_name.clone()).collect(), fingerprint: cycle.iter().map(|r| r.signature.as_str()).collect::<Vec<_>>().join("\u{1}") });
        }
    }
    None
}
struct GateEntry { fingerprint: String, last_notified_count: usize, saturation_notified: bool }
#[derive(Default)]
pub struct NoticeGate { entries: BTreeMap<LoopGuardKind, GateEntry> }
impl NoticeGate {
    pub fn admit(&mut self, detection: &LoopGuardDetection) -> bool {
        let maximum = detection.maximum_count();
        let count = detection.count();
        if let Some(existing) = self.entries.get_mut(&detection.kind())
            && existing.fingerprint == detection.fingerprint() {
            let saturation = !existing.saturation_notified && count >= maximum;
            if count >= existing.last_notified_count * ESCALATION_FACTOR || saturation {
                existing.last_notified_count = count;
                if saturation { existing.saturation_notified = true; }
                return true;
            }
            return false;
        }
        self.entries.insert(detection.kind(), GateEntry { fingerprint: detection.fingerprint().into(), last_notified_count: count, saturation_notified: count >= maximum });
        true
    }
    pub fn prune(&mut self, active: &BTreeMap<LoopGuardKind, String>) { self.entries.retain(|kind, entry| active.get(kind).is_some_and(|fp| *fp == entry.fingerprint)); }
    pub fn reset(&mut self) { self.entries.clear(); }
}
pub fn detect_loop(records: &[ToolCallRecord], gate: &mut NoticeGate) -> Option<LoopGuardDetection> {
    let detections = [detect_identical_run(records), detect_cycle(records), detect_similar_run(records)];
    let active = detections.iter().flatten().map(|d| (d.kind(), d.fingerprint().into())).collect();
    gate.prune(&active);
    detections.into_iter().flatten().find(|d| gate.admit(d))
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::tracker::ToolCallTracker;
    fn calls(names: &[&str]) -> Vec<ToolCallRecord> { let mut tracker = ToolCallTracker::default(); for name in names { tracker.record(name, None); } tracker.records().to_vec() }
    #[test] fn identical_below_threshold_is_absent() { let records = calls(&["read", "read"]); let result = detect_identical_run(&records); assert!(result.is_none()); }
    #[test] fn identical_reports_full_trailing_run() { let records = calls(&["bash", "read", "read", "read"]); let result = detect_identical_run(&records).unwrap(); assert_eq!(result.count(), 3); }
    #[test] fn identical_run_counts_key_order_insensitive_duplicates() {
        let mut tracker=ToolCallTracker::default();
        for source in ["{\"path\":\"a.ts\",\"limit\":50}","{\"limit\":50,\"path\":\"a.ts\"}","{\"path\":\"a.ts\",\"limit\":50}"] {
            let args=serde_json::from_str(source).unwrap(); tracker.record("read",Some(&args));
        }
        assert_eq!(detect_identical_run(tracker.records()).unwrap().count(),3);
    }
    #[test] fn two_cycles_are_below_threshold() { let records = calls(&["a", "b", "a", "b"]); let result = detect_cycle(&records); assert!(result.is_none()); }
    #[test] fn period_two_cycle_fires() { let records = calls(&["a", "b", "a", "b", "a", "b"]); let result = detect_cycle(&records); assert!(matches!(result, Some(LoopGuardDetection::Cycle { period: 2, count: 3, .. }))); }
    #[test] fn leading_prefix_does_not_mask_cycle() { let records = calls(&["seed", "a", "b", "a", "b", "a", "b"]); let result = detect_cycle(&records); assert!(result.is_some()); }
    #[test] fn identical_window_is_not_cycle() { let records = calls(&["a"; 12]); let result = detect_cycle(&records); assert!(result.is_none()); }
    #[test] fn period_three_cycle_fires() { let records = calls(&["a", "b", "c", "a", "b", "c", "a", "b", "c"]); let result = detect_cycle(&records); assert!(matches!(result, Some(LoopGuardDetection::Cycle { period: 3, count: 3, .. }))); }
    #[test] fn intermediate_counts_are_suppressed() { let mut gate = NoticeGate::default(); let first = detect_loop(&calls(&["read"; 3]), &mut gate); let middle = detect_loop(&calls(&["read"; 5]), &mut gate); let doubled = detect_loop(&calls(&["read"; 6]), &mut gate); assert!(first.is_some()); assert!(middle.is_none()); assert_eq!(doubled.unwrap().count(), 6); }
    #[test] fn broken_pattern_rearms_notice() { let mut gate = NoticeGate::default(); detect_loop(&calls(&["a"; 3]), &mut gate); detect_loop(&calls(&["a", "a", "a", "b"]), &mut gate); let result = detect_loop(&calls(&["a", "a", "a", "b", "a", "a", "a"]), &mut gate); assert!(result.is_some()); }
    #[test] fn identical_is_preferred() { let mut gate = NoticeGate::default(); let result = detect_loop(&calls(&["read"; 5]), &mut gate); assert_eq!(result.unwrap().kind(), LoopGuardKind::Identical); }
    #[test] fn distinct_read_targets_are_exempt() { let mut tracker = ToolCallTracker::default(); for i in 0..6 { tracker.record("read", Some(&serde_json::json!({"path":format!("src/similar-long-filename-{i}.rs")}))); } let result = detect_similar_run(tracker.records()); assert!(result.is_none()); }
    #[test] fn pagination_same_target_is_detected() { let mut tracker = ToolCallTracker::default(); for i in 0..6 { tracker.record("read", Some(&serde_json::json!({"path":"src/similar-long-filename.rs", "offset":i * 200, "limit":200}))); } let result = detect_similar_run(tracker.records()); assert!(result.is_some()); }
    #[test] fn saturation_notifies_once() { let mut gate = NoticeGate::default(); for count in [3, 6, 12, 24, 48] { assert!(gate.admit(&LoopGuardDetection::Identical { tool_name: "read".into(), count, fingerprint: "fp".into() })); } let detection = LoopGuardDetection::Identical { tool_name: "read".into(), count: 64, fingerprint: "fp".into() }; let result = (gate.admit(&detection), gate.admit(&detection)); assert_eq!(result, (true, false)); }
}
