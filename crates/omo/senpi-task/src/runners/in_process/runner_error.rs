//! Typed runner failure (`runners/in-process/runner-error.ts`).

use crate::host::HostError;
use crate::runners::in_process::child_handle::{RunnerFailure, RunnerFailureKind};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", failure.message)]
pub struct RunnerError {
    pub failure: RunnerFailure,
    pub cause: Option<HostError>,
}

impl RunnerError {
    pub fn new(kind: RunnerFailureKind, message: impl Into<String>) -> Self {
        Self {
            failure: RunnerFailure::new(kind, message),
            cause: None,
        }
    }

    pub fn caused_by(
        kind: RunnerFailureKind,
        message: impl Into<String>,
        cause: HostError,
    ) -> Self {
        Self {
            failure: RunnerFailure::new(kind, message),
            cause: Some(cause),
        }
    }

    pub fn kind(&self) -> RunnerFailureKind {
        self.failure.kind
    }
}
