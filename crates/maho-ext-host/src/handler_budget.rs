//! Port of senpi packages/coding-agent/src/core/extensions/handler-budget.ts (senpi#3650).
//!
//! Host budget for extension event handlers. Past `warn_ms` the host reports a handler as stalled; at
//! `timeout_ms`, or when the turn deadline releases the turn it belongs to, the host stops waiting on it and
//! moves on. A value of 0 disables that limit.
//!
//! Native difference: a JavaScript handler the host stops waiting on keeps running detached and a late
//! rejection is reported as a `late_failure` stall. A native handler future borrows the emission's event and
//! context, so the host drops it instead: the handler is cancelled at its current await point and has no
//! late outcome to report.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// senpi `DEFAULT_EXTENSION_EVENT_HANDLER_WARN_MS`.
pub const DEFAULT_EXTENSION_EVENT_HANDLER_WARN_MS: u64 = 60_000;
/// senpi `DEFAULT_EXTENSION_EVENT_HANDLER_TIMEOUT_MS`.
pub const DEFAULT_EXTENSION_EVENT_HANDLER_TIMEOUT_MS: u64 = 1_200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionHandlerBudget {
    pub warn_ms: u64,
    pub timeout_ms: u64,
}

impl ExtensionHandlerBudget {
    /// senpi `UNBOUNDED_HANDLER_BUDGET`, the runner's default until the host sets a budget.
    pub const UNBOUNDED: Self = Self {
        warn_ms: 0,
        timeout_ms: 0,
    };
    pub const DEFAULT: Self = Self {
        warn_ms: DEFAULT_EXTENSION_EVENT_HANDLER_WARN_MS,
        timeout_ms: DEFAULT_EXTENSION_EVENT_HANDLER_TIMEOUT_MS,
    };
    pub const fn is_unbounded(self) -> bool {
        self.warn_ms == 0 && self.timeout_ms == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionHandlerStallStage {
    Warn,
    Timeout,
    Released,
    LateFailure,
}

impl ExtensionHandlerStallStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Timeout => "timeout",
            Self::Released => "released",
            Self::LateFailure => "late_failure",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionHandlerStall {
    pub extension_path: String,
    pub event: String,
    pub stage: ExtensionHandlerStallStage,
    pub elapsed_ms: u64,
    pub timeout_ms: u64,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerStopVerdict {
    TimedOut,
    Released,
}

pub enum BudgetedHandlerOutcome<T> {
    Settled(T),
    Stopped(HandlerStopVerdict),
}

pub type HandlerStallListener = Arc<dyn Fn(&ExtensionHandlerStall) + Send + Sync>;
/// Resolves the budget for one emission. `Err` is an invalid setting: reported once, then the defaults apply.
pub type HandlerBudgetResolver =
    Arc<dyn Fn() -> Result<ExtensionHandlerBudget, String> + Send + Sync>;
pub type HandlerTurnScope = Arc<dyn Fn() -> u64 + Send + Sync>;

struct HeldWait {
    id: u64,
    scope: u64,
    release: tokio::sync::oneshot::Sender<()>,
}

/// Waits on running handlers, tagged with the turn that owns them so a deadline releases only its own turn's waits.
#[derive(Default)]
pub struct HeldHandlerWaits {
    next_id: Mutex<u64>,
    waits: Mutex<Vec<HeldWait>>,
}

/// Removes its wait when the handler settles first.
pub struct HeldWaitGuard<'a> {
    waits: &'a HeldHandlerWaits,
    id: u64,
}

impl Drop for HeldWaitGuard<'_> {
    fn drop(&mut self) {
        self.waits
            .waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|wait| wait.id != self.id);
    }
}

impl HeldHandlerWaits {
    pub fn hold(&self, scope: u64) -> (HeldWaitGuard<'_>, tokio::sync::oneshot::Receiver<()>) {
        let (release, released) = tokio::sync::oneshot::channel();
        let id = {
            let mut next = self
                .next_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *next += 1;
            *next
        };
        self.waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(HeldWait { id, scope, release });
        (HeldWaitGuard { waits: self, id }, released)
    }

    /// Releases every wait held by `scope`, in hold order; returns how many were released.
    pub fn release(&self, scope: u64) -> usize {
        let owned = {
            let mut waits = self
                .waits
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (owned, kept): (Vec<_>, Vec<_>) =
                waits.drain(..).partition(|wait| wait.scope == scope);
            *waits = kept;
            owned
        };
        let count = owned.len();
        for wait in owned {
            let _receiver_gone = wait.release.send(());
        }
        count
    }
}

/// Runs one handler future under the budget. A future that settles in time is returned exactly as a direct
/// `.await` would; one past its cap or released by the turn deadline is dropped (see the module note).
pub async fn run_budgeted_handler<T>(
    extension_path: &str,
    event: &str,
    budget: ExtensionHandlerBudget,
    held: Option<(&HeldHandlerWaits, u64)>,
    on_stall: Option<&HandlerStallListener>,
    handler: impl Future<Output = T>,
) -> BudgetedHandlerOutcome<T> {
    if budget.is_unbounded() && held.is_none() {
        return BudgetedHandlerOutcome::Settled(handler.await);
    }
    let started_at = Instant::now();
    let stall = |stage: ExtensionHandlerStallStage| {
        if let Some(listener) = on_stall {
            listener(&ExtensionHandlerStall {
                extension_path: extension_path.to_owned(),
                event: event.to_owned(),
                stage,
                elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                timeout_ms: budget.timeout_ms,
                error: None,
            });
        }
    };
    let (_guard, released) = match held {
        Some((waits, scope)) => {
            let (guard, released) = waits.hold(scope);
            (Some(guard), Some(released))
        }
        None => (None, None),
    };
    let released = async move {
        match released {
            // A dropped sender (the waits were cleared) is not a release.
            Some(released) => {
                if released.await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(handler);
    tokio::pin!(released);
    let warn = tokio::time::sleep(Duration::from_millis(budget.warn_ms));
    tokio::pin!(warn);
    let cap = tokio::time::sleep(Duration::from_millis(budget.timeout_ms));
    tokio::pin!(cap);
    let mut warned = false;
    loop {
        tokio::select! {
            biased;
            value = &mut handler => return BudgetedHandlerOutcome::Settled(value),
            () = &mut released => {
                stall(ExtensionHandlerStallStage::Released);
                return BudgetedHandlerOutcome::Stopped(HandlerStopVerdict::Released);
            }
            () = &mut cap, if budget.timeout_ms > 0 => {
                stall(ExtensionHandlerStallStage::Timeout);
                return BudgetedHandlerOutcome::Stopped(HandlerStopVerdict::TimedOut);
            }
            () = &mut warn, if budget.warn_ms > 0 && !warned => {
                warned = true;
                stall(ExtensionHandlerStallStage::Warn);
            }
        }
    }
}

/// senpi `reportStoppedHandler`'s error text.
pub fn stopped_handler_error(
    verdict: HandlerStopVerdict,
    budget: ExtensionHandlerBudget,
) -> String {
    match verdict {
        HandlerStopVerdict::TimedOut => format!("handler timed out after {}ms", budget.timeout_ms),
        HandlerStopVerdict::Released => "handler released by the turn deadline".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_only_the_waits_of_the_given_turn() {
        let waits = HeldHandlerWaits::default();
        let (_a, mut turn_1_a) = waits.hold(1);
        let (_b, mut turn_2) = waits.hold(2);
        let (_c, mut turn_1_b) = waits.hold(1);

        assert_eq!(waits.release(1), 2);
        assert!(turn_1_a.try_recv().is_ok());
        assert!(turn_1_b.try_recv().is_ok());
        assert!(turn_2.try_recv().is_err());
        assert_eq!(waits.release(2), 1);
        assert!(turn_2.try_recv().is_ok());
    }

    #[test]
    fn a_settled_wait_is_no_longer_held() {
        let waits = HeldHandlerWaits::default();
        let (guard, _released) = waits.hold(1);
        drop(guard);
        assert_eq!(waits.release(1), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_a_handler_that_never_settles_and_reports_the_stall() {
        let stalls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&stalls);
        let listener: HandlerStallListener =
            Arc::new(move |stall: &ExtensionHandlerStall| seen.lock().unwrap().push(stall.stage));
        let budget = ExtensionHandlerBudget {
            warn_ms: 0,
            timeout_ms: 100,
        };
        let outcome = run_budgeted_handler(
            "ext",
            "agent_start",
            budget,
            None,
            Some(&listener),
            std::future::pending::<()>(),
        )
        .await;
        assert!(matches!(
            outcome,
            BudgetedHandlerOutcome::Stopped(HandlerStopVerdict::TimedOut)
        ));
        assert_eq!(
            *stalls.lock().unwrap(),
            [ExtensionHandlerStallStage::Timeout]
        );
        assert_eq!(
            stopped_handler_error(HandlerStopVerdict::TimedOut, budget),
            "handler timed out after 100ms"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_turn_deadline_releases_a_held_handler_without_a_budget() {
        let waits = HeldHandlerWaits::default();
        let run = run_budgeted_handler(
            "ext",
            "turn_end",
            ExtensionHandlerBudget::UNBOUNDED,
            Some((&waits, 7)),
            None,
            std::future::pending::<()>(),
        );
        tokio::pin!(run);
        // The handler is held: poll once so the wait is registered, then release turn 7.
        assert!(futures_poll_once(run.as_mut()).await.is_none());
        assert_eq!(waits.release(7), 1);
        assert!(matches!(
            run.await,
            BudgetedHandlerOutcome::Stopped(HandlerStopVerdict::Released)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timely_handler_settles_with_its_value_and_reports_no_stall() {
        let stalls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&stalls);
        let listener: HandlerStallListener =
            Arc::new(move |stall: &ExtensionHandlerStall| seen.lock().unwrap().push(stall.stage));
        let outcome = run_budgeted_handler(
            "ext",
            "message_end",
            ExtensionHandlerBudget {
                warn_ms: 50,
                timeout_ms: 100,
            },
            None,
            Some(&listener),
            async { 42 },
        )
        .await;
        assert!(matches!(outcome, BudgetedHandlerOutcome::Settled(42)));
        assert!(stalls.lock().unwrap().is_empty());
    }

    async fn futures_poll_once<F: Future + Unpin>(future: F) -> Option<F::Output> {
        let mut future = future;
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(match std::pin::Pin::new(&mut future).poll(cx) {
                std::task::Poll::Ready(value) => Some(value),
                std::task::Poll::Pending => None,
            })
        })
        .await
    }
}
