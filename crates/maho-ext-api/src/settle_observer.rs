//! Port of the `SettleObserver` handle from `omo-senpi/src/extension/types.ts` (omo v5.1.52).
//!
//! Upstream passes the observer as the third argument of an `agent_settled` handler. The native
//! handler signature is fixed, so the bounded wrapper (maho-omo `SettleObservers`) installs it as a
//! task-local for the handler's future instead; handlers read it with [`current_settle_observer`].
//! It lives here, not in maho-omo, so memory components can read it without a dependency cycle.

use std::sync::{Arc, Mutex, PoisonError};

use maho_ai::utils::abort::AbortSignal;

use crate::ExtensionFailure;

/// What an `agent_settled` handler may use to cooperate with the bounded observation.
#[derive(Clone)]
pub struct SettleObserver {
    signal: AbortSignal,
    hook: Arc<Mutex<String>>,
}

impl SettleObserver {
    pub fn new(signal: AbortSignal, hook_label: impl Into<String>) -> Self {
        Self {
            signal,
            hook: Arc::new(Mutex::new(hook_label.into())),
        }
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }

    /// The label of the phase currently awaited (the hook label until a phase is named).
    pub fn hook_label(&self) -> String {
        self.hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Names the current await and refuses to begin another phase after retirement.
    pub fn phase(&self, label: &str) -> Result<(), ExtensionFailure> {
        if let Some(reason) = self.signal.reason() {
            return Err(ExtensionFailure::from_abort_reason(reason));
        }
        *self.hook.lock().unwrap_or_else(PoisonError::into_inner) = label.to_owned();
        Ok(())
    }

    /// The observer signal as a tool-layer signal, for APIs that take `maho_tools::AbortSignal`.
    pub fn tool_signal(&self) -> crate::AbortSignal {
        let bridged = crate::AbortSignal::default();
        if self.signal.aborted() {
            bridged.abort();
        } else {
            let target = bridged.clone();
            self.signal.add_abort_listener(move |_| target.abort());
            // An abort between the check and the registration drops the listener; re-check.
            if self.signal.aborted() {
                bridged.abort();
            }
        }
        bridged
    }
}

tokio::task_local! {
    static CURRENT_SETTLE_OBSERVER: SettleObserver;
}

/// The observer of the `agent_settled` handler running on this task, if any.
pub fn current_settle_observer() -> Option<SettleObserver> {
    CURRENT_SETTLE_OBSERVER.try_with(Clone::clone).ok()
}

/// Runs `future` with `observer` as the current settle observer.
pub async fn with_settle_observer<F: std::future::Future>(
    observer: SettleObserver,
    future: F,
) -> F::Output {
    CURRENT_SETTLE_OBSERVER.scope(observer, future).await
}

#[cfg(test)]
mod tests {
    use maho_ai::utils::abort::{AbortController, AbortReason};

    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn current_observer_is_scoped_to_the_future() {
        assert!(current_settle_observer().is_none());
        let controller = AbortController::new();
        let observer = SettleObserver::new(controller.signal(), "onSettled");
        let seen = block_on(with_settle_observer(observer, async {
            current_settle_observer().map(|o| o.hook_label())
        }));
        assert_eq!(seen.as_deref(), Some("onSettled"));
        assert!(current_settle_observer().is_none());
    }

    #[test]
    fn phase_names_the_hook_until_aborted() {
        let controller = AbortController::new();
        let observer = SettleObserver::new(controller.signal(), "onSettled");
        assert!(observer.phase("journal").is_ok());
        assert_eq!(observer.hook_label(), "journal");
        let reason = AbortReason::new("AbortError", "retired").with_payload(7_u8);
        controller.abort(Some(reason.clone()));
        let refused = observer
            .phase("next")
            .expect_err("retired observer refuses a phase");
        assert_eq!(refused.message, "retired");
        assert_eq!(refused.abort_reason.as_deref(), Some(&reason));
        assert_eq!(observer.hook_label(), "journal");
    }

    #[test]
    fn tool_signal_follows_the_observer_signal() {
        let controller = AbortController::new();
        let observer = SettleObserver::new(controller.signal(), "onSettled");
        let bridged = observer.tool_signal();
        assert!(!bridged.is_aborted());
        controller.abort(None);
        assert!(bridged.is_aborted());
        assert!(observer.tool_signal().is_aborted());
    }
}
