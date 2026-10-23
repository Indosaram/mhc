//! The window a subscription request gets (senpi `context-window.ts`).

/// Claude Code's default window when a request does not ask for the long context.
pub const CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// Models whose 1M window is reachable only through Claude Code's `[1m]` opt-in. Every other model above
/// 200k has a native 1M window and is requested by its plain id (senpi#3801).
pub const LEGACY_LONG_CONTEXT_MODEL_IDS: [&str; 5] = [
    "claude-3-7-sonnet",
    "claude-opus-4-6",
    "claude-sonnet-4-0",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
];

fn opens_legacy_long_context(id: &str, context_window: u64) -> bool {
    context_window > CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW
        && LEGACY_LONG_CONTEXT_MODEL_IDS.contains(&id)
}

/// The window a request gets, which is what compaction plans against: only a legacy id loses 1M on a refusal.
pub fn planned_context_window(id: &str, context_window: u64, long_context: bool) -> u64 {
    if opens_legacy_long_context(id, context_window) && !long_context {
        CLAUDE_CODE_DEFAULT_CONTEXT_WINDOW
    } else {
        context_window
    }
}

/// `planned_context_window` against the persisted refusal state.
pub fn planned_context_window_now(id: &str, context_window: u64) -> u64 {
    planned_context_window(
        id,
        context_window,
        crate::long_context::long_context_available(),
    )
}

/// The model id Claude Code is given: `[1m]` only for legacy ids while the account has not refused it.
pub fn sdk_model_id(id: &str, context_window: u64, long_context: bool) -> String {
    if opens_legacy_long_context(id, context_window) && long_context {
        format!("{id}[1m]")
    } else {
        id.to_owned()
    }
}

pub fn sdk_model_id_now(id: &str, context_window: u64) -> String {
    sdk_model_id(
        id,
        context_window,
        crate::long_context::long_context_available(),
    )
}

/// Whether a built request asked for the legacy long-context window, the only request it can be refused for.
pub fn asks_legacy_long_context(sdk_model: Option<&str>) -> bool {
    sdk_model.is_some_and(|model| model.ends_with("[1m]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_legacy_ids_use_the_opt_in() {
        assert_eq!(
            sdk_model_id("claude-sonnet-4-5", 1_000_000, true),
            "claude-sonnet-4-5[1m]"
        );
        assert_eq!(
            sdk_model_id("claude-sonnet-4-5", 1_000_000, false),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            sdk_model_id("claude-opus-5-5", 1_000_000, true),
            "claude-opus-5-5"
        );
        assert_eq!(
            sdk_model_id("claude-sonnet-4-5", 200_000, true),
            "claude-sonnet-4-5"
        );
    }
    #[test]
    fn refusal_caps_only_legacy_ids() {
        assert_eq!(
            planned_context_window("claude-opus-4-6", 1_000_000, false),
            200_000
        );
        assert_eq!(
            planned_context_window("claude-opus-4-6", 1_000_000, true),
            1_000_000
        );
        assert_eq!(
            planned_context_window("claude-opus-5-5", 1_000_000, false),
            1_000_000
        );
        assert!(asks_legacy_long_context(Some("claude-opus-4-6[1m]")));
        assert!(!asks_legacy_long_context(Some("claude-opus-4-6")));
        assert!(!asks_legacy_long_context(None));
    }
}
