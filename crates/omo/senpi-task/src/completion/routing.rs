use super::types::{ParentState, RoutingDecision, TransitionReason};
use crate::state::TaskStatus;

/// Only externally-caused terminals (completed/error/lost) notify. Parent-initiated
/// cancel/interrupt return synchronously in the tool result.
pub fn should_notify_status(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Error | TaskStatus::Lost
    )
}

/// Delivery is unconditional: idle always wakes, streaming always steers into the running turn,
/// transient transitions buffer until the next session_start/idle edge.
pub fn route_completion(parent_state: ParentState) -> RoutingDecision {
    match parent_state {
        ParentState::Idle => RoutingDecision::Wake,
        ParentState::Streaming => RoutingDecision::DeliverStreaming,
        ParentState::Compacting => RoutingDecision::Buffer(TransitionReason::Compacting),
        ParentState::SessionSwitching => {
            RoutingDecision::Buffer(TransitionReason::SessionSwitching)
        }
        ParentState::SessionShutdown => RoutingDecision::Buffer(TransitionReason::SessionShutdown),
    }
}
