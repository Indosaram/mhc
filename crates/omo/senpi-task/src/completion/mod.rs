//! Completion push to the parent session (`completion/` in TypeScript).

mod notification;
mod notifier;
mod routing;
mod types;

pub use notification::{
    BuildDetailsOptions, FINAL_RESPONSE_TRANSPORT_LIMIT, build_completion_details,
    build_completion_message, completion_message_lines,
};
pub use notifier::{CompletionNotifier, create_completion_notifier};
pub use routing::{route_completion, should_notify_status};
pub use types::{
    COMPLETION_CUSTOM_TYPE, CompletionDetails, CompletionNotifierDeps, CompletionNotifierStore,
    CompletionRequest, CompletionRetrySchedule, DeliveredDecision, DeliveryCallbacks, DeliveryState, FlushInput, FlushResult,
    NotifyResult, ParentNotifier, ParentNotifierMessage, ParentState,
    ReconcileUnnotifiedNotificationsInput, RoutingDecision, ScheduledCancel, ScheduledTask,
    SkipReason, TransitionReason,
};

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
