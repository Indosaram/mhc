//! Startup windows for the RPC MCP inventory (senpi#4828).
//!
//! While a session's MCP attach is running, and until every connect that attach backgrounded has settled (connected,
//! failed, or timed out by its own `connectTimeoutMs`), inventory changes for that session are held instead of being
//! announced one by one. How many intermediate inventories a capture happens to see depends on how parallel connects
//! interleave and on how often a client reads the inventory, so announcing them made the startup event count depend on
//! timing. When the session's last open window closes it releases what it held exactly once: a `startup-settled`
//! announcement when any of its attaches started a connect, otherwise the plain change it withheld, if any.
use futures::{FutureExt, future::BoxFuture};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupWindowRelease {
    StartupSettled,
    Withheld,
}

#[derive(Default)]
struct WindowState {
    open: usize,
    connects: usize,
    withheld: bool,
}

pub struct McpStartupWindow {
    session_id: String,
    state: Arc<Mutex<WindowState>>,
    windows: Arc<Mutex<BTreeMap<String, Arc<Mutex<WindowState>>>>>,
    closed: bool,
}

impl McpStartupWindow {
    /// Close after `connects` settle; `release` runs once, for the session's last open window, when it held anything.
    pub async fn close<F, Fut>(self, connects: Vec<BoxFuture<'static, ()>>, release: F)
    where
        F: FnOnce(StartupWindowRelease) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let task = tokio::spawn(async move {
            if std::panic::AssertUnwindSafe(self.close_inner(connects, release))
                .catch_unwind()
                .await
                .is_err()
            {
                maho_ext_api::diagnostic("MCP startup window close panicked");
            }
        });
        if let Err(error) = task.await {
            maho_ext_api::diagnostic(&format!("MCP startup window close failed: {error}"));
        }
    }

    async fn close_inner<F, Fut>(mut self, connects: Vec<BoxFuture<'static, ()>>, release: F)
    where
        F: FnOnce(StartupWindowRelease) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.connects += connects.len();
        }
        let _ = futures::future::join_all(connects).await;
        if let Some(kind) = self.finish() {
            release(kind).await;
        }
    }

    fn finish(&mut self) -> Option<StartupWindowRelease> {
        if self.closed {
            return None;
        }
        self.closed = true;
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.open = state.open.saturating_sub(1);
        let is_owner = windows
            .get(&self.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.state));
        if state.open > 0 || !is_owner {
            return None;
        }
        windows.remove(&self.session_id);
        if state.connects > 0 {
            Some(StartupWindowRelease::StartupSettled)
        } else if state.withheld {
            Some(StartupWindowRelease::Withheld)
        } else {
            None
        }
    }
}

impl Drop for McpStartupWindow {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Default)]
pub struct McpStartupWindows {
    windows: Arc<Mutex<BTreeMap<String, Arc<Mutex<WindowState>>>>>,
}

impl McpStartupWindows {
    pub fn open(&self, session_id: &str) -> McpStartupWindow {
        let state = {
            let mut windows = self
                .windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let state = windows
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(WindowState::default())))
                .clone();
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .open += 1;
            state
        };
        McpStartupWindow {
            session_id: session_id.to_owned(),
            state,
            windows: self.windows.clone(),
            closed: false,
        }
    }

    /// True while `session_id` has an open window; the change it was asked about is then held for the release.
    pub fn hold(&self, session_id: Option<&str>) -> bool {
        let Some(session_id) = session_id else {
            return false;
        };
        let state = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned();
        let Some(state) = state else {
            return false;
        };
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .withheld = true;
        true
    }

    pub fn clear(&self) {
        self.windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}
