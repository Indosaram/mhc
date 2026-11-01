//! Process-wide pooled `reqwest::Client` for provider HTTP send paths.
//!
//! PERF-DIVERGENCE(senpi packages/ai/src/api/anthropic.ts): upstream builds a new SDK client per request but Bun fetch shares a keep-alive agent; this port reuses one pooled reqwest client. Requests are identical.

use std::sync::LazyLock;
use std::time::Duration;

fn build() -> reqwest::Client {
    reqwest::Client::builder()
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

static SHARED: LazyLock<reqwest::Client> = LazyLock::new(build);

/// One keep-alive connection pool shared by every provider request (clones share the pool).
pub fn shared_http_client() -> reqwest::Client {
    SHARED.clone()
}
