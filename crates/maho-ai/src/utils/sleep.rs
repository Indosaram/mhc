//! Port of senpi packages/ai/src/utils/sleep.ts.

use super::abort::{AbortReason, AbortSignal};
use std::time::Duration;

pub async fn sleep(ms: u64, signal: &AbortSignal) -> Result<(), AbortReason> {
    signal.throw_if_aborted()?;
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(signal.reason().unwrap_or_else(AbortReason::dom_default)),
        () = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;

    #[tokio::test(start_paused = true)]
    async fn resolves_after_delay() {
        let controller = AbortController::new();
        assert_eq!(sleep(50, &controller.signal()).await, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn rejects_with_reason_on_abort() {
        let controller = AbortController::new();
        controller.abort(Some(AbortReason::new("Error", "x")));
        assert_eq!(sleep(50, &controller.signal()).await, Err(AbortReason::new("Error", "x")));
    }
}
