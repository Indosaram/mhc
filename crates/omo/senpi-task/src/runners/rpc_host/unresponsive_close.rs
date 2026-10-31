//! `runners/rpc-host/unresponsive-close.ts`: reacquire only this session to close it; never
//! resume a turn or signal its shared host.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use super::handle_port::{HostSessionIdentity, HostSessionPort};
use super::session_wire::host_session_unresponsive_error;
use crate::host::HostError;

/// The abort before the confirmed close waits at most this long (`min(closeGrace, 2 s)`).
const ABORT_BOUND: Duration = Duration::from_millis(2_000);

/// The reattach seam: reopen ONLY this session; `Ok(None)` when the host refused it.
pub type UnresponsiveReattach = Arc<
    dyn Fn(
            &HostSessionIdentity,
        )
            -> Result<Option<(Arc<dyn HostSessionPort>, HostSessionIdentity, bool)>, HostError>
        + Send
        + Sync,
>;

pub struct UnresponsiveCloseInput {
    pub task_id: String,
    pub close_grace: Duration,
    pub reattach: Option<UnresponsiveReattach>,
    pub port: Arc<dyn Fn() -> Arc<dyn HostSessionPort> + Send + Sync>,
    pub session: Arc<dyn Fn() -> HostSessionIdentity + Send + Sync>,
    pub stopped: Arc<dyn Fn() -> bool + Send + Sync>,
    pub adopt: Arc<dyn Fn(Arc<dyn HostSessionPort>, HostSessionIdentity) + Send + Sync>,
}

/// The probe answered that the original live session is gone (`unknown_session`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresponsiveProbe {
    State(Value),
    Missing,
}

pub struct UnresponsiveClose {
    input: UnresponsiveCloseInput,
    disconnected: AtomicBool,
    reopened: AtomicBool,
    reconnecting: Mutex<()>,
    stop_requested: AtomicBool,
}

pub fn create_unresponsive_close(input: UnresponsiveCloseInput) -> UnresponsiveClose {
    UnresponsiveClose {
        input,
        disconnected: AtomicBool::new(false),
        reopened: AtomicBool::new(false),
        reconnecting: Mutex::new(()),
        stop_requested: AtomicBool::new(false),
    }
}

fn detached(operation: &str) -> HostError {
    HostError {
        message: format!("the host session holds no connection ({operation})"),
    }
}

fn close_port(port: &dyn HostSessionPort) -> Result<(), HostError> {
    port.close_confirmed()
        .unwrap_or_else(|| Err(host_session_unresponsive_error()))
}

impl UnresponsiveClose {
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    pub fn transport_lost(&self) {
        self.disconnected.store(true, Ordering::Release);
    }

    fn stopped(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire) || (self.input.stopped)()
    }

    fn connect(&self) -> Result<(), HostError> {
        let _single = self
            .reconnecting
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !self.disconnected.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(reattach) = &self.input.reattach else {
            return Err(detached("close_session"));
        };
        let Some((port, session, attached)) = reattach(&(self.input.session)())? else {
            return Err(detached("close_session"));
        };
        if (self.input.stopped)() {
            return port.detach();
        }
        (self.input.adopt)(port, session);
        self.reopened.store(!attached, Ordering::Release);
        self.disconnected.store(false, Ordering::Release);
        Ok(())
    }

    /// The heartbeat probe while not running: reconnect first; a reopened transcript is closed and
    /// reported missing, never re-prompted.
    pub fn probe(&self) -> Result<UnresponsiveProbe, HostError> {
        self.connect()?;
        let port = (self.input.port)();
        if self.reopened.load(Ordering::Acquire) {
            close_port(port.as_ref())?;
            return Ok(UnresponsiveProbe::Missing);
        }
        port.get_state().map(UnresponsiveProbe::State)
    }

    /// The terminal-pending close: a bounded abort, then the confirmed close.
    pub fn close(&self) -> Result<(), HostError> {
        self.connect()?;
        if self.stopped() {
            return Ok(());
        }
        let port = (self.input.port)();
        let bound = self.input.close_grace.min(ABORT_BOUND);
        let (sent_tx, sent_rx) = std::sync::mpsc::channel();
        let abort_port = Arc::clone(&port);
        std::thread::spawn(move || {
            let _ = sent_tx.send(abort_port.send(json!({ "type": "abort" })));
        });
        match sent_rx.recv_timeout(bound) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log_abort_pending(&self.input.task_id, &error.message),
            Err(_) => log_abort_pending(&self.input.task_id, "abort was not acknowledged in time"),
        }
        if self.stopped() {
            return Ok(());
        }
        // Parent shutdown's best-effort helper swallows this failure. Liveness cannot.
        close_port(port.as_ref())
    }
}

fn log_abort_pending(task_id: &str, error: &str) {
    utils::logger::log(
        "senpi-task unresponsive session abort pending",
        Some(&json!({ "taskId": task_id, "error": error })),
    );
}
