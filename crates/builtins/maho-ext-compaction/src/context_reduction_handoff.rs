//! Port of senpi's compaction `context-reduction-handoff.ts`.
use maho_ext_api::{
    AbortSource, CompactOptions, CustomMessage, DeliverAs, ExtensionApi, ExtensionContext,
    ExtensionFailure, SendMessageOptions, ToolContent,
};
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

pub fn create_reduction_handoff(
    api: Arc<ExtensionApi>,
    custom_instructions: &str,
) -> ReductionHandoff {
    ReductionHandoff {
        api,
        custom_instructions: custom_instructions.to_owned(),
        active: Arc::default(),
    }
}

impl ReductionHandoff {
    pub fn is_pending(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
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
            let request = ActiveHandoff {
                session_id: context.session_manager.session_id().to_owned(),
                leaf: context.session_manager.get_leaf_id(),
            };
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
            let (active, api, context) = (
                Arc::clone(&self.active),
                Arc::clone(&self.api),
                context.clone(),
            );
            Arc::new(move |_: maho_ext_api::CompactionResult| {
                {
                    let mut active = active.lock().unwrap_or_else(PoisonError::into_inner);
                    if active.as_ref() != Some(&request) {
                        return;
                    }
                    *active = None;
                }
                let moved = context.session_manager.session_id() != request.session_id
                    || request.leaf.as_ref().is_some_and(|leaf| {
                        !context
                            .session_manager
                            .get_branch()
                            .iter()
                            .any(|entry| &entry.id == leaf)
                    });
                if moved {
                    return;
                }
                match context.has_pending_messages() {
                    Ok(true) => return,
                    Ok(false) => {}
                    // An unreadable context is the stale-runtime case: the session moved on and this
                    // handoff no longer owns it. Say so instead of returning as if work were queued.
                    Err(error) => {
                        let _ = api.events.try_emit(
                            "senpi.context-reduction.resume-skipped",
                            &serde_json::json!({"message": error.message}),
                        );
                        return;
                    }
                }
                // A failed follow-up must not surface as a compaction failure; the user can
                // continue manually. It is not silent, though: a runtime replaced between the
                // compaction and this call refuses the send with a stale-ctx failure, and that is
                // exactly the case where the session would otherwise look stuck for minutes.
                if let Err(error) = api.send_message(
                    CustomMessage {
                        custom_type: CONTEXT_REDUCTION_RESUME_TYPE.into(),
                        content: vec![ToolContent::text(
                            "Continue the current task using the compacted context.",
                        )],
                        display: false,
                        details: None,
                    },
                    SendMessageOptions {
                        trigger_turn: true,
                        deliver_as: Some(DeliverAs::FollowUp),
                    },
                ) {
                    // The bus may be stale for the same reason the send was refused; the core
                    // drains any wake parked this way once a live runtime is installed again.
                    let _ = api.events.try_emit(
                        "senpi.context-reduction.resume-refused",
                        &serde_json::json!({"message": error.message}),
                    );
                }
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
