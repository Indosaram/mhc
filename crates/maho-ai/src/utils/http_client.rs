//! Pooled `reqwest::Client` for provider HTTP send paths, one per tokio runtime.
//!
//! PERF-DIVERGENCE(senpi packages/ai/src/api/anthropic.ts): upstream builds a new SDK client per request but Bun fetch shares a keep-alive agent; this port reuses one pooled reqwest client. Requests are identical.
//!
//! The pool is per runtime because a pooled connection's I/O is driven by the runtime that
//! opened it: mhc runs several runtimes in one process (in-process child agents, memory and
//! task hosts), and a connection checked out after its runtime shut down fails the request.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::runtime::{Handle, Id};

/// Bound on remembered runtimes; past it the map is cleared (clients of dropped runtimes are
/// only reclaimed then, and live ones are rebuilt on their next request).
const MAX_RUNTIMES: usize = 64;

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

static CLIENTS: LazyLock<Mutex<HashMap<Id, reqwest::Client>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The keep-alive connection pool of the current tokio runtime (clones share the pool).
/// Outside a runtime it returns a fresh client.
pub fn shared_http_client() -> reqwest::Client {
    let Ok(handle) = Handle::try_current() else {
        return build();
    };
    let mut clients = CLIENTS.lock().unwrap_or_else(|error| error.into_inner());
    if clients.len() >= MAX_RUNTIMES && !clients.contains_key(&handle.id()) {
        clients.clear();
    }
    clients.entry(handle.id()).or_insert_with(build).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_client_per_runtime() {
        let runtime = || tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let (first, second) = (runtime(), runtime());
        let first_id = first.handle().id();
        let second_id = second.handle().id();
        first.block_on(async { shared_http_client() });
        second.block_on(async { shared_http_client() });
        let clients = CLIENTS.lock().expect("clients");
        assert!(clients.contains_key(&first_id));
        assert!(clients.contains_key(&second_id));
    }
}
