//! Port of senpi `packages/coding-agent/src/core/pending-input-public.ts`.
//!
//! The public pending-input lifecycle (senpi#3549): the one reducer, its projection and closed types, and the
//! store over an injected journal. Other owners embed this exact module instead of reimplementing it.
//! Re-exports only, and nothing bound to the session file.

pub use crate::pending_input::reduce_pending_input;
pub use crate::pending_input_projection::{
    PendingInputAction, PendingInputProjection, PendingInputState, PendingInputStatusCode,
    project_pending_input,
};
pub use crate::pending_input_store::{
    PENDING_INPUT_TRANSITION, PendingInputError, PendingInputJournal, PendingInputJournalRecord,
    PendingInputListener, PendingInputRead, PendingInputStore, PendingInputStoreOptions,
    PendingInputTransaction, StopItem, StopReceipt,
};
pub use crate::pending_input_types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_the_reducer_the_projection_and_the_store() {
        let decision = reduce_pending_input(None, &PendingInputEvent::Teardown);
        assert_eq!(decision.next, None);
        assert_eq!(PENDING_INPUT_TRANSITION, "pending_input_transition");
        let _: fn(&PendingInput) -> PendingInputProjection = project_pending_input;
        let _: Option<std::sync::Arc<PendingInputStore>> = None;
    }
}
