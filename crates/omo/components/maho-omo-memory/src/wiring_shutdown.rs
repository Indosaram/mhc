//! Session-shutdown drain choreography (`wiring-shutdown.ts`, #11671, 2a55e3024b).

use std::future::Future;
use std::time::Duration;

use crate::shutdown_drain::ShutdownReason;

/// Races an auxiliary settle step against the remaining shutdown deadline.
///
/// The step keeps running detached if the timeout fires first, matching upstream `raceDetached`.
pub async fn race_detached_step<Fut, W>(
    name: &str,
    deadline: f64,
    now: &(dyn Fn() -> f64 + Sync),
    warn: &mut W,
    work: Fut,
) where
    Fut: Future<Output = ()> + Send + 'static,
    W: FnMut(&str, Option<&str>) + ?Sized,
{
    let remaining_ms = (deadline - now()).max(0.0);
    if remaining_ms == 0.0 {
        // Still start detached so background state releases cleanly, but record deadline reached.
        drop(tokio::spawn(work));
        warn(name, None);
        return;
    }
    let remaining = Duration::from_secs_f64((remaining_ms / 1000.0).min(u64::MAX as f64));
    let mut handle = tokio::spawn(work);
    tokio::select! {
        biased;
        result = &mut handle => {
            if let Err(error) = result {
                warn(name, Some(&error.to_string()));
            }
        }
        () = tokio::time::sleep(remaining) => {
            warn(name, None);
        }
    }
}

pub fn should_run_quit_only_steps(reason: ShutdownReason) -> bool {
    reason == ShutdownReason::Quit
}
