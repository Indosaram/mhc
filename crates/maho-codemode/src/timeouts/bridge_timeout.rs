pub use crate::bridge::reserved::{TIMEOUT_PAUSE_OP, TIMEOUT_RESUME_OP};
use super::idle_timeout::TimeoutPauseHandle;

pub async fn with_bridge_timeout_pause<T>(watchdog: Option<&dyn TimeoutPauseHandle>, operation: impl std::future::Future<Output = T>) -> T {
    struct Resume<'a>(Option<&'a dyn TimeoutPauseHandle>);
    impl Drop for Resume<'_> {
        fn drop(&mut self) {
            if let Some(watchdog) = self.0 { watchdog.resume(); }
        }
    }
    if let Some(watchdog) = watchdog { watchdog.pause(); }
    let _resume = Resume(watchdog);
    operation.await
}
