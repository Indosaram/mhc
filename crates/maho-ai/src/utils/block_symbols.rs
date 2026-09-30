//! Port of senpi packages/ai/src/utils/block-symbols.ts.
//!
//! TS symbol-keyed markers never persist, so they are side-car state carried next to a streamed
//! content block rather than properties on it.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamingBlockState {
    pub partial_json: Option<String>,
    pub block_index: Option<usize>,
    pub last_parse_len: Option<usize>,
    /// Cursor interaction envelope `call_id`, distinct from the block id the exec channel pairs results under.
    pub envelope_id: Option<String>,
    pub block_kind: Option<String>,
    /// The Cursor exec channel already executed this tool call; the agent loop must not run it again.
    pub cursor_exec_resolved: bool,
}

pub fn clear_streaming_partial_json(state: &mut StreamingBlockState) {
    if state.partial_json.is_some() {
        state.partial_json = None;
    }
}

pub fn is_cursor_exec_resolved(state: Option<&StreamingBlockState>) -> bool {
    state.is_some_and(|s| s.cursor_exec_resolved)
}

pub fn copy_cursor_exec_resolved(target: &mut StreamingBlockState, source: &StreamingBlockState) {
    if source.cursor_exec_resolved {
        target.cursor_exec_resolved = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_only_partial_json() {
        let mut state = StreamingBlockState { partial_json: Some("{\"a\":1".into()), block_index: Some(2), ..StreamingBlockState::default() };
        clear_streaming_partial_json(&mut state);
        assert_eq!(state.partial_json, None);
        assert_eq!(state.block_index, Some(2));
    }

    #[test]
    fn cursor_exec_resolved_defaults_to_false_and_copies_forward() {
        assert!(!is_cursor_exec_resolved(None));
        let unresolved = StreamingBlockState::default();
        assert!(!is_cursor_exec_resolved(Some(&unresolved)));

        let resolved = StreamingBlockState { cursor_exec_resolved: true, ..StreamingBlockState::default() };
        assert!(is_cursor_exec_resolved(Some(&resolved)));

        let mut target = StreamingBlockState::default();
        copy_cursor_exec_resolved(&mut target, &unresolved);
        assert!(!target.cursor_exec_resolved);
        copy_cursor_exec_resolved(&mut target, &resolved);
        assert!(target.cursor_exec_resolved);
    }
}
