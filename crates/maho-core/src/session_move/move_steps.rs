//! Durable move boundaries. Hooks are task-local, so concurrent tests cannot interfere.
use std::sync::Arc;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveStep {
    ImportReceived,
    ImportStamped,
    ImportRenamed,
    ImportAccepted,
    RevokeSealed,
    RevokeMarked,
    FinishRecorded,
    FinishRequeued,
}
pub const MOVE_STEPS: &[MoveStep] = &[
    MoveStep::ImportReceived,
    MoveStep::ImportStamped,
    MoveStep::ImportRenamed,
    MoveStep::ImportAccepted,
    MoveStep::RevokeSealed,
    MoveStep::RevokeMarked,
    MoveStep::FinishRecorded,
    MoveStep::FinishRequeued,
];
pub type MoveStepHook = Arc<dyn Fn(MoveStep) -> Result<(), String> + Send + Sync>;
tokio::task_local! { static HOOK: MoveStepHook; }
pub async fn with_move_step_hook<T>(
    hook: MoveStepHook,
    work: impl std::future::Future<Output = T>,
) -> T {
    HOOK.scope(hook, work).await
}
pub fn move_step(step: MoveStep) -> Result<(), String> {
    HOOK.try_with(|hook| hook(step)).unwrap_or(Ok(()))
}
