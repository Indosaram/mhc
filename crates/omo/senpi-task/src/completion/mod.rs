//! Completion push to the parent session (`completion/` in TypeScript).

mod notification;
mod notifier;
mod routing;
mod types;

pub mod dag_verification_directive;
pub use notification::{
    BuildDetailsOptions, FINAL_RESPONSE_TRANSPORT_LIMIT, build_completion_details,
    build_completion_message, completion_message_lines, describe_parent_model_fallback,
};
pub use notifier::{CompletionNotifier, create_completion_notifier};
pub use routing::{route_completion, should_notify_status};
pub use types::{
    COMPLETION_CUSTOM_TYPE, CompletionDetails, CompletionNotifierDeps, CompletionNotifierStore,
    CompletionRequest, CompletionRetrySchedule, DeliveredDecision, DeliveryCallbacks, DeliveryState, FlushInput, FlushResult,
    NotifyResult, ParentModelFallback, ParentModelFallbackPhase, ParentModelFallbackReason,
    ParentNotifier, ParentNotifierMessage, ParentState,
    ReconcileUnnotifiedNotificationsInput, RoutingDecision, RunRef, RunRefKind, ScheduledCancel, ScheduledTask,
    SkipReason, TransitionReason,
};

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
