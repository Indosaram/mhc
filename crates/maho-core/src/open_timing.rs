//! Port of senpi packages/coding-agent/src/core/open-timing.ts.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::brand::env_value;

pub fn open_timing_enabled() -> bool {
    env_value("OPEN_TIMING", &crate::config::current_env()).as_deref() == Some("1")
}

tokio::task_local! {
    static SPECULATIVE: bool;
}

pub async fn without_open_timing<T, F>(build: F) -> T
where
    F: std::future::Future<Output = T>,
{
    SPECULATIVE.scope(true, build).await
}

pub fn is_speculative_open() -> bool {
    SPECULATIVE.try_with(|v| *v).unwrap_or(false)
}

pub struct OpenTimer {
    scope: Option<String>,
    last: Instant,
}

impl OpenTimer {
    pub fn start(scope: impl Into<String>) -> Self {
        if !open_timing_enabled() || is_speculative_open() {
            return Self {
                scope: None,
                last: Instant::now(),
            };
        }
        Self {
            scope: Some(scope.into()),
            last: Instant::now(),
        }
    }

    pub fn mark(&mut self, phase: &str) {
        let Some(ref scope) = self.scope else {
            return;
        };
        let now = Instant::now();
        let delta_ms = now.duration_since(self.last).as_millis();
        self.last = now;
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        crate::output_guard::maho_write_stderr(&format!(
            "senpi open-timing {epoch_ms} {scope} {phase} +{delta_ms}ms\n"
        ));
    }
}
