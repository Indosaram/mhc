//! Isolated experimental entry point corresponding to upstream pico3/index.ts.
pub use super::bounded::Bounded;
pub use super::chord::{
    ChordViewBridge, ChordViewBridgeOptions, PicoConversationService, PublishedConversationView,
    attach_chord_view, attach_chord_view_with, create_pico_conversation_service,
    pico_conversation_service, pico_harness_service,
};
pub use super::harness::{
    BuiltinKinds, ConversationHandle, Harness, HarnessOptions, InputHandle, Unregister,
    capture_active_transcript, entries, is_core_kind,
};
pub use super::jsonl::JsonlStorage;
pub use super::memory::MemoryStorage;
pub use super::system::{SystemSection, define_system_section, system_sections};
pub use super::types::*;
pub use super::view::{WATCH_CAPACITY, Watch, apply_envelope};
pub use crate::harness::context::with_abort_signal;

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
