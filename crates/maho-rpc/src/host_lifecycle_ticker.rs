//! The supervisor's idle and lifetime-owner ticker (senpi `rpc/host-lifecycle-ticker.ts`): idle and owner checks
//! share one ticker and never overlap owner probes.
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// What the ticker reads from the supervisor (senpi `SupervisorActivity` / `SupervisorOwner`).
pub trait SupervisorTickerHost: Send + Sync + 'static {
    /// `true` when the idle decision says exit and no client is still unclassified.
    fn idle_exit_due(&self) -> bool;
    fn draining(&self) -> bool;
    fn shutting_down(&self) -> bool;
    /// Whether the lifetime owner is gone; a probe that may take time.
    fn owner_gone(&self) -> impl Future<Output = bool> + Send;
    fn shutdown(&self, reason: &'static str, exit_code: i32);
}

/// The tick: a quarter of the idle window, clamped to 20 ms..1 s.
pub fn tick_interval_ms(idle_exit_ms: f64) -> u64 {
    (idle_exit_ms / 4.0).clamp(20.0, 1_000.0) as u64
}

/// Starts the ticker; the returned handle's `abort` stops it (senpi returns a stop closure that also stops the owner).
pub fn start_supervisor_ticker<H: SupervisorTickerHost>(
    idle_exit_ms: f64,
    host: Arc<H>,
) -> tokio::task::JoinHandle<()> {
    let period = Duration::from_millis(tick_interval_ms(idle_exit_ms));
    let owner_check = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if !host.draining() && host.idle_exit_due() {
                host.shutdown("idle", 0);
            }
            if !host.shutting_down()
                && !host.draining()
                && !owner_check.swap(true, Ordering::SeqCst)
            {
                let host = Arc::clone(&host);
                let owner_check = Arc::clone(&owner_check);
                tokio::spawn(async move {
                    let gone = host.owner_gone().await;
                    if gone && !host.shutting_down() && !host.draining() {
                        host.shutdown("owner_gone", 0);
                    }
                    owner_check.store(false, Ordering::SeqCst);
                });
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Host {
        idle: AtomicBool,
        owner_gone: AtomicBool,
        draining: AtomicBool,
        shutdowns: Mutex<Vec<&'static str>>,
        notify: tokio::sync::Notify,
    }
    impl SupervisorTickerHost for Host {
        fn idle_exit_due(&self) -> bool {
            self.idle.load(Ordering::SeqCst)
        }
        fn draining(&self) -> bool {
            self.draining.load(Ordering::SeqCst)
        }
        fn shutting_down(&self) -> bool {
            !self
                .shutdowns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        }
        async fn owner_gone(&self) -> bool {
            self.owner_gone.load(Ordering::SeqCst)
        }
        fn shutdown(&self, reason: &'static str, _exit_code: i32) {
            self.shutdowns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(reason);
            self.notify.notify_one();
        }
    }

    #[test]
    fn the_tick_is_a_clamped_quarter_window() {
        assert_eq!(tick_interval_ms(40.0), 20);
        assert_eq!(tick_interval_ms(2_000.0), 500);
        assert_eq!(tick_interval_ms(60_000.0), 1_000);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_and_owner_loss_shut_down_unless_draining() {
        let host = Arc::new(Host::default());
        host.idle.store(true, Ordering::SeqCst);
        host.draining.store(true, Ordering::SeqCst);
        let ticker = start_supervisor_ticker(100.0, Arc::clone(&host));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !host.shutting_down(),
            "a draining supervisor never idles out"
        );
        host.draining.store(false, Ordering::SeqCst);
        host.notify.notified().await;
        assert_eq!(
            host.shutdowns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[0],
            "idle"
        );
        ticker.abort();

        let owner = Arc::new(Host::default());
        owner.owner_gone.store(true, Ordering::SeqCst);
        let ticker = start_supervisor_ticker(100.0, Arc::clone(&owner));
        owner.notify.notified().await;
        assert_eq!(
            owner
                .shutdowns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[0],
            "owner_gone"
        );
        ticker.abort();
    }
}
