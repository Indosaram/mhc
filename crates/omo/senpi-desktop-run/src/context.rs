//! Native Rust port of `src/run/context.ts`.
//!
//! Provides the `ComputerRunError`, `RunOutput`, `RunContext`, and facade method guarding.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use senpi_desktop_protocol::{
    ComputerCallPolicy, ComputerDisplay, ComputerScreenshot, ComputerSessionSnapshot,
};
use senpi_desktop_service::rpc_client::DesktopError;
use serde_json::Value;

/// Why a computer run failed inside the runtime; engine failures surface as `DesktopEngineRpcError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ComputerRunErrorReason {
    #[error("read-only run: exec method requires read_only: false")]
    ReadOnly,
    #[error("computer run timed out")]
    Timeout,
    #[error("computer run was aborted")]
    Aborted,
    #[error("computer run has ended")]
    Ended,
    #[error("assertion failed")]
    Assertion,
    #[error("window selector error")]
    Window,
    #[error("wait predicate error")]
    Wait,
}

impl ComputerRunErrorReason {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "readOnly",
            Self::Timeout => "timeout",
            Self::Aborted => "aborted",
            Self::Ended => "ended",
            Self::Assertion => "assertion",
            Self::Window => "window",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ComputerRunError {
    pub reason: ComputerRunErrorReason,
    pub message: String,
}

impl ComputerRunError {
    pub fn new(reason: ComputerRunErrorReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }

    pub fn read_only(method: &str) -> Self {
        Self {
            reason: ComputerRunErrorReason::ReadOnly,
            message: format!("read-only run: '{method}' requires read_only: false"),
        }
    }

    pub fn timeout(timeout_ms: u64) -> Self {
        Self {
            reason: ComputerRunErrorReason::Timeout,
            message: format!("computer run timed out after {timeout_ms} ms"),
        }
    }

    pub fn aborted() -> Self {
        Self {
            reason: ComputerRunErrorReason::Aborted,
            message: "computer run was aborted".to_string(),
        }
    }

    pub fn ended() -> Self {
        Self {
            reason: ComputerRunErrorReason::Ended,
            message: "computer run has ended".to_string(),
        }
    }

    pub fn assertion(message: Option<&str>) -> Self {
        Self {
            reason: ComputerRunErrorReason::Assertion,
            message: message.unwrap_or("Assertion failed").to_string(),
        }
    }

    pub fn window(message: impl Into<String>) -> Self {
        Self {
            reason: ComputerRunErrorReason::Window,
            message: message.into(),
        }
    }

    pub fn wait(message: impl Into<String>) -> Self {
        Self {
            reason: ComputerRunErrorReason::Wait,
            message: message.into(),
        }
    }
}

/// Unified error that a facade method or input action can produce:
/// either a runtime check refusal / semantic error (`ComputerRun`), or an engine RPC / transport failure (`Engine`).
#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum DesktopRunError {
    #[error(transparent)]
    ComputerRun(#[from] ComputerRunError),
    #[error(transparent)]
    Engine(#[from] DesktopError),
}

/// Ordered text and image entries shown to the model; an accumulator, mutable by design.
#[derive(Debug, Default, Clone)]
pub struct RunOutput {
    entries: Arc<Mutex<Vec<ComputerDisplay>>>,
}

impl RunOutput {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn text(&self, text: impl Into<String>) {
        self.entries
            .lock()
            .await
            .push(ComputerDisplay::Text { text: text.into() });
    }

    pub async fn image(&self, data: impl Into<String>, mime_type: impl Into<String>) {
        self.entries
            .lock()
            .await
            .push(ComputerDisplay::Image {
                data: data.into(),
                mime_type: mime_type.into(),
            });
    }

    pub async fn finish(&self) -> Vec<ComputerDisplay> {
        std::mem::take(&mut *self.entries.lock().await)
    }
}

/// The state one run owns. Each run gets its own context and a facade bound to this object.
#[derive(Clone)]
pub struct RunContext {
    pub aborted: Arc<AtomicBool>,
    pub abort_reason: Arc<Mutex<Option<ComputerRunError>>>,
    pub read_only: bool,
    pub snapshot: ComputerSessionSnapshot,
    pub output: RunOutput,
    pub screenshots: Arc<Mutex<Vec<ComputerScreenshot>>>,
}

impl RunContext {
    pub fn new(snapshot: ComputerSessionSnapshot) -> Self {
        let read_only = snapshot.read_only;
        Self {
            aborted: Arc::new(AtomicBool::new(false)),
            abort_reason: Arc::new(Mutex::new(None)),
            read_only,
            snapshot,
            output: RunOutput::new(),
            screenshots: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn abort(&self, error: ComputerRunError) {
        if !self.aborted.swap(true, Ordering::SeqCst) {
            *self.abort_reason.lock().await = Some(error);
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    pub async fn check_aborted(&self) -> Result<(), ComputerRunError> {
        if self.is_aborted() {
            let reason = self.abort_reason.lock().await;
            if let Some(err) = reason.as_ref() {
                return Err(err.clone());
            }
            return Err(ComputerRunError::aborted());
        }
        Ok(())
    }
}

/// One engine request on behalf of the run, carrying the run cancellation signal.
pub type EngineCall = Arc<
    dyn Fn(
            &'static str,
            Value,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Value, DesktopError>> + Send,
            >,
        > + Send
        + Sync,
>;

/// Tier table of one facade surface (`DESKTOP_METHODS`, `WINDOW_METHODS`, `ELEMENT_METHODS`).
pub type MethodTiers = &'static [(&'static str, ComputerCallPolicy)];

/// What every facade method needs: the run it belongs to and its engine channel.
#[derive(Clone)]
pub struct RunScope {
    pub context: RunContext,
    pub call: EngineCall,
}

impl RunScope {
    pub fn new(context: RunContext, call: EngineCall) -> Self {
        Self { context, call }
    }
}

/// Admits one facade call: refuses `exec` methods in a read-only run and anything after the run ended.
/// A method missing from `tiers` counts as `exec`, so a new helper fails closed.
pub async fn guard_run(
    context: &RunContext,
    tiers: MethodTiers,
    method: &str,
) -> Result<(), ComputerRunError> {
    let tier = tiers
        .iter()
        .find(|(name, _)| *name == method)
        .map(|(_, policy)| *policy)
        .unwrap_or(ComputerCallPolicy::Exec);

    if context.read_only && tier == ComputerCallPolicy::Exec {
        return Err(ComputerRunError::read_only(method));
    }
    context.check_aborted().await
}
