//! Port of senpi core/extensions/builtin/handoff-hint.
pub mod brief;
pub mod estimate;
pub mod index;
pub use index::{
    DEFAULT_HANDOFF_HINT_SETTINGS, HANDOFF_BRIEF_MESSAGE_TYPE, HANDOFF_COMMAND,
    HANDOFF_HINT_ENTRY_TYPE, HANDOFF_HINT_EVENT, HANDOFF_HINT_WIDGET_KEY, HandoffHint,
    HandoffHintAction, HandoffHintSettings, KEEP_GOING_LABEL, START_FRESH_LABEL,
};
