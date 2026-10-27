//! Port of senpi's compaction `context-reduction-state.ts`.
use serde_json::{Value, json};

pub const CONTEXT_REDUCTION_ENTRY_TYPE: &str = "senpi.context-reduction.v1";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextReductionState {
    pub engaged: bool,
    pub cut_index: usize,
    /// The cut before the last frontier step; its cache checkpoint stays marked so the prefix bytes never change between steps (senpi#3242).
    pub checkpoint_cut: Option<usize>,
    pub prefix_hash: String,
    pub anchor_count: Option<usize>,
    pub anchor_cut: Option<usize>,
    pub anchor_hash: Option<String>,
    pub anchor_tokens: Option<u64>,
    pub compaction_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextReductionRequest {
    pub count: usize,
    pub cut: usize,
    pub hash: String,
}

pub fn create_context_reduction_state() -> ContextReductionState {
    ContextReductionState::default()
}

/// The persisted form: `compactionRequired` is request-local and never saved.
pub fn snapshot_reduction_state(state: &ContextReductionState) -> Value {
    let mut snapshot = json!({"engaged":state.engaged,"cutIndex":state.cut_index,"prefixHash":state.prefix_hash});
    if let Some(cut) = state.checkpoint_cut {
        snapshot["checkpointCut"] = json!(cut);
    }
    if let (Some(count), Some(cut), Some(hash), Some(tokens)) = (state.anchor_count, state.anchor_cut, state.anchor_hash.as_ref(), state.anchor_tokens) {
        snapshot["anchorCount"] = json!(count);
        snapshot["anchorCut"] = json!(cut);
        snapshot["anchorHash"] = json!(hash);
        snapshot["anchorTokens"] = json!(tokens);
    }
    snapshot
}

pub fn clear_reduction_anchor(state: &mut ContextReductionState) {
    state.anchor_count = None;
    state.anchor_cut = None;
    state.anchor_hash = None;
    state.anchor_tokens = None;
}

pub fn record_reduction_usage(state: &mut ContextReductionState, request: &ContextReductionRequest, tokens: f64) {
    if !state.engaged || !tokens.is_finite() || tokens <= 0. || state.compaction_required || request.cut != state.cut_index || request.hash != state.prefix_hash {
        return;
    }
    state.anchor_count = Some(request.count);
    state.anchor_cut = Some(request.cut);
    state.anchor_hash = Some(request.hash.clone());
    state.anchor_tokens = Some(tokens.ceil() as u64);
}

fn safe_index(value: &Value, key: &str) -> Option<usize> {
    value.get(key)?.as_u64().map(|number| number as usize)
}

pub fn read_context_reduction_state(data: &Value) -> Option<ContextReductionState> {
    let engaged = data.get("engaged")?.as_bool()?;
    let cut_index = safe_index(data, "cutIndex")?;
    let prefix_hash = data.get("prefixHash")?.as_str()?.to_owned();
    let mut state = ContextReductionState { engaged, cut_index, prefix_hash, ..Default::default() };
    if data.get("checkpointCut").is_some() {
        let checkpoint_cut = safe_index(data, "checkpointCut")?;
        if checkpoint_cut >= cut_index {
            return None;
        }
        state.checkpoint_cut = Some(checkpoint_cut);
    }
    if ["anchorCount", "anchorCut", "anchorHash", "anchorTokens"].iter().any(|key| data.get(key).is_some()) {
        let count = safe_index(data, "anchorCount")?;
        let cut = safe_index(data, "anchorCut")?;
        let hash = data.get("anchorHash")?.as_str()?.to_owned();
        let tokens = data.get("anchorTokens")?.as_f64().filter(|tokens| tokens.is_finite() && *tokens >= 0.)?;
        if cut > count {
            return None;
        }
        state.anchor_count = Some(count);
        state.anchor_cut = Some(cut);
        state.anchor_hash = Some(hash);
        state.anchor_tokens = Some(tokens.ceil() as u64);
    }
    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engaged() -> ContextReductionState {
        ContextReductionState { engaged: true, cut_index: 4, prefix_hash: "h".into(), ..Default::default() }
    }
    fn request(cut: usize, hash: &str) -> ContextReductionRequest {
        ContextReductionRequest { count: 9, cut, hash: hash.into() }
    }

    #[test]
    fn snapshot_never_persists_compaction_required_and_round_trips() {
        let mut state = ContextReductionState { checkpoint_cut: Some(2), compaction_required: true, ..engaged() };
        state.compaction_required = false;
        record_reduction_usage(&mut state, &request(4, "h"), 123.2);
        state.compaction_required = true;
        let snapshot = snapshot_reduction_state(&state);
        assert!(snapshot.get("compactionRequired").is_none());
        let restored = read_context_reduction_state(&snapshot).expect("valid snapshot");
        assert_eq!(restored, ContextReductionState { compaction_required: false, anchor_tokens: Some(124), ..state });
    }

    #[test]
    fn usage_anchors_only_the_request_that_matches_the_live_frontier() {
        for (mut state, request) in [
            (engaged(), request(3, "h")),
            (engaged(), request(4, "other")),
            (ContextReductionState { engaged: false, ..engaged() }, request(4, "h")),
            (ContextReductionState { compaction_required: true, ..engaged() }, request(4, "h")),
        ] {
            record_reduction_usage(&mut state, &request, 50.);
            assert_eq!(state.anchor_tokens, None);
        }
        for tokens in [0., -1., f64::NAN, f64::INFINITY] {
            let mut state = engaged();
            record_reduction_usage(&mut state, &request(4, "h"), tokens);
            assert_eq!(state.anchor_tokens, None);
        }
        let mut state = engaged();
        record_reduction_usage(&mut state, &request(4, "h"), 50.);
        assert_eq!((state.anchor_count, state.anchor_cut, state.anchor_hash.as_deref(), state.anchor_tokens), (Some(9), Some(4), Some("h"), Some(50)));
        clear_reduction_anchor(&mut state);
        assert_eq!(state.anchor_tokens, None);
    }

    #[test]
    fn reader_rejects_corrupt_records() {
        let base = json!({"engaged":true,"cutIndex":4,"prefixHash":"h"});
        assert!(read_context_reduction_state(&base).is_some());
        for patch in [
            json!({"engaged":"yes"}), json!({"cutIndex":-1}), json!({"cutIndex":1.5}), json!({"prefixHash":3}),
            json!({"checkpointCut":4}), json!({"checkpointCut":"1"}),
            json!({"anchorCount":5}),
            json!({"anchorCount":3,"anchorCut":4,"anchorHash":"h","anchorTokens":1}),
            json!({"anchorCount":5,"anchorCut":4,"anchorHash":"h","anchorTokens":-1}),
        ] {
            let mut record = base.clone();
            for (key, value) in patch.as_object().expect("patch object") {
                record[key] = value.clone();
            }
            assert!(read_context_reduction_state(&record).is_none(), "{patch}");
        }
        assert!(read_context_reduction_state(&json!(null)).is_none());
    }
}
