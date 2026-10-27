//! Port of senpi's compaction `context-reduction-handoff.ts`.
use maho_ext_api::{AbortSource, CompactOptions, CustomMessage, DeliverAs, ExtensionApi, ExtensionContext, ExtensionFailure, SendMessageOptions, ToolContent};
use std::sync::{Arc, Mutex, PoisonError};

pub const CONTEXT_REDUCTION_RESUME_TYPE: &str = "senpi.context-reduction.resume";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveHandoff {
    session_id: String,
    leaf: Option<String>,
}

/// Owns only the current branch's in-flight handoff, never a persisted request.
#[derive(Clone)]
pub struct ReductionHandoff {
    api: Arc<ExtensionApi>,
    custom_instructions: String,
    active: Arc<Mutex<Option<ActiveHandoff>>>,
}

pub fn create_reduction_handoff(api: Arc<ExtensionApi>, custom_instructions: &str) -> ReductionHandoff {
    ReductionHandoff { api, custom_instructions: custom_instructions.to_owned(), active: Arc::default() }
}

impl ReductionHandoff {
    pub fn is_pending(&self) -> bool {
        self.active.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    pub fn reset(&self) {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub fn request(&self, context: &ExtensionContext) -> Result<(), ExtensionFailure> {
        let request = {
            let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            if active.is_some() {
                drop(active);
                return context.abort(Some(AbortSource::System));
            }
            let request = ActiveHandoff { session_id: context.session_manager.session_id().to_owned(), leaf: context.session_manager.get_leaf_id() };
            *active = Some(request.clone());
            request
        };
        let on_error = {
            let (active, request) = (Arc::clone(&self.active), request.clone());
            Arc::new(move |_: ExtensionFailure| {
                let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
                if active.as_ref() == Some(&request) {
                    *active = None;
                }
            })
        };
        let on_complete = {
            let (active, api, context) = (Arc::clone(&self.active), Arc::clone(&self.api), context.clone());
            Arc::new(move |_: maho_ext_api::CompactionResult| {
                {
                    let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
                    if active.as_ref() != Some(&request) {
                        return;
                    }
                    *active = None;
                }
                let moved = context.session_manager.session_id() != request.session_id
                    || request.leaf.as_ref().is_some_and(|leaf| !context.session_manager.get_branch().iter().any(|entry| &entry.id == leaf));
                if moved || context.has_pending_messages().unwrap_or(true) {
                    return;
                }
                // A failed follow-up must not surface as a compaction failure; the
                // user can continue manually.
                let _ = api.send_message(
                    CustomMessage {
                        custom_type: CONTEXT_REDUCTION_RESUME_TYPE.into(),
                        content: vec![ToolContent::text("Continue the current task using the compacted context.")],
                        display: false,
                        details: None,
                    },
                    SendMessageOptions { trigger_turn: true, deliver_as: Some(DeliverAs::FollowUp) },
                );
            })
        };
        let result = context.compact(CompactOptions {
            custom_instructions: Some(self.custom_instructions.clone()),
            on_complete: Some(on_complete),
            on_error: Some(on_error),
        });
        if result.is_err() {
            // `compact` never reached the session, so no callback will release the latch.
            self.reset();
        }
        result
    }
}
