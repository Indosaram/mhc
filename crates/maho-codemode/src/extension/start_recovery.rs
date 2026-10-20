use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_ext_api::ExtensionFailure;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub type StartFuture = Pin<Box<dyn Future<Output = Result<(), ExtensionFailure>> + Send>>;
pub type StartSession<C> = Arc<dyn Fn(Value, C) -> StartFuture + Send + Sync>;
type AttemptResult = Result<(), CodemodeRuntimeRecreationError>;
type Attempt = tokio::sync::watch::Receiver<Option<AttemptResult>>;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "codemode runtime could not be re-created: {0}. Start a new session or reload to bring eval back."
)]
pub struct CodemodeRuntimeRecreationError(pub String);

struct RecoveryState {
    failed_event: Option<Value>,
    attempt: Option<Attempt>,
    controller: AbortController,
}

/// One recreation per failed session start; concurrent evals observe the same result.
pub struct StartRecovery<C> {
    start_session: StartSession<C>,
    report: Arc<dyn Fn(&str) + Send + Sync>,
    state: Mutex<RecoveryState>,
}

impl<C: Send + 'static> StartRecovery<C> {
    pub fn new(start_session: StartSession<C>, report: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        Self {
            start_session,
            report,
            state: Mutex::new(RecoveryState {
                failed_event: None,
                attempt: None,
                controller: AbortController::new(),
            }),
        }
    }

    pub async fn run_start(&self, event: Value, context: C) -> Result<(), ExtensionFailure> {
        let signal = {
            let mut state = self.state.lock().expect("startup recovery state");
            state.controller.abort(None);
            state.controller = AbortController::new();
            state.failed_event = None;
            state.attempt = None;
            state.controller.signal()
        };
        let result =
            run_until_ended((self.start_session)(event.clone(), context), signal.clone()).await;
        let mut state = self.state.lock().expect("startup recovery state");
        if !signal.aborted() {
            state.failed_event = result.as_ref().err().map(|_| event);
        }
        result
    }

    pub fn session_ended(&self) {
        let mut state = self.state.lock().expect("startup recovery state");
        state.controller.abort(None);
        state.failed_event = None;
        state.attempt = None;
    }

    /// Returns false when no failed start authorizes recovery. A dropped eval does
    /// not cancel the shared attempt; ending the session does.
    pub async fn recover(
        &self,
        context: C,
        session_id: &str,
    ) -> Result<bool, CodemodeRuntimeRecreationError> {
        let mut attempt = {
            let mut state = self.state.lock().expect("startup recovery state");
            match &state.attempt {
                Some(attempt) => attempt.clone(),
                None => {
                    let Some(event) = state.failed_event.take() else {
                        return Ok(false);
                    };
                    let hash = format!("{:x}", Sha256::digest(session_id.as_bytes()));
                    (self.report)(&format!(
                        "[senpi-codemode] eval re-created a runtime left disposed by a failed session_start (session {})",
                        &hash[..12]
                    ));
                    let (sender, receiver) = tokio::sync::watch::channel(None);
                    let operation = (self.start_session)(event, context);
                    let signal = state.controller.signal();
                    tokio::spawn(async move {
                        let result = run_until_ended(operation, signal)
                            .await
                            .map_err(|error| CodemodeRuntimeRecreationError(error.to_string()));
                        sender.send_replace(Some(result));
                    });
                    state.attempt = Some(receiver.clone());
                    receiver
                }
            }
        };
        let result = attempt
            .wait_for(Option::is_some)
            .await
            .map_err(|error| CodemodeRuntimeRecreationError(error.to_string()))?
            .clone();
        match result {
            Some(result) => {
                if result.is_ok() {
                    let mut state = self.state.lock().expect("startup recovery state");
                    if state.attempt.as_ref().is_some_and(|current| current.same_channel(&attempt)) {
                        state.attempt = None;
                    }
                }
                result.map(|()| true)
            }
            None => unreachable!("watch predicate requires a settled attempt"),
        }
    }
}

async fn run_until_ended(
    operation: StartFuture,
    signal: AbortSignal,
) -> Result<(), ExtensionFailure> {
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(ExtensionFailure::new("codemode session manager is disposed")),
        result = operation => result,
    }
}
