use std::fmt;

pub use crate::daemon_client::{
    CallToolOptions, DaemonToolContext, ToolExecutionResult, call_diagnostics_via_daemon,
    call_format_via_daemon, call_tool_via_daemon, current_request_context,
};
pub use crate::runtime_contract::{
    DaemonRuntime, DaemonRuntimeDefaults, Env, InvalidRuntimeOverrideError,
    OMO_LSP_DAEMON_CLI, OMO_LSP_DAEMON_VERSION,
};
use crate::runtime_contract::{DaemonRuntimeError, InvalidRuntimeOverrideReason};

pub const OMO_LSP_DAEMON_DIR: &str = "OMO_LSP_DAEMON_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRuntimeError {
    Override(InvalidRuntimeOverrideError),
    Version(String),
}

impl fmt::Display for ClientRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Override(error) => error.fmt(formatter),
            Self::Version(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ClientRuntimeError {}

pub fn validate_daemon_version(version: &str) -> Result<String, ClientRuntimeError> {
    crate::runtime_contract::validate_daemon_version(version)
        .map_err(|error| ClientRuntimeError::Version(error.to_string()))
}

pub fn resolve_daemon_runtime(
    env: &Env,
    defaults: &DaemonRuntimeDefaults,
) -> Result<DaemonRuntime, ClientRuntimeError> {
    crate::runtime_contract::resolve_daemon_runtime(env, defaults).map_err(|error| match error {
        DaemonRuntimeError::Version(error) => ClientRuntimeError::Version(error.to_string()),
        DaemonRuntimeError::Override(mut error) => {
            match error.reason {
                InvalidRuntimeOverrideReason::CliNotFound => {
                    error.message = format!("{OMO_LSP_DAEMON_CLI} points to a missing file");
                }
                InvalidRuntimeOverrideReason::CliNotFile => {
                    error.message = format!("{OMO_LSP_DAEMON_CLI} must point to a regular file");
                }
                InvalidRuntimeOverrideReason::PairedValuesRequired
                | InvalidRuntimeOverrideReason::CliMustBeAbsolute
                | InvalidRuntimeOverrideReason::PackagedCliMustBeAbsolute => {}
            }
            ClientRuntimeError::Override(error)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_runtime_keeps_its_distinct_errors() {
        let root = tempfile::tempdir().expect("temporary runtime directory");
        let defaults = DaemonRuntime {
            cli_path: root.path().join("packaged"),
            version: "1".to_string(),
        };
        for (path, reason, message) in [
            (root.path().join("missing"), InvalidRuntimeOverrideReason::CliNotFound,
             "OMO_LSP_DAEMON_CLI points to a missing file"),
            (root.path().to_path_buf(), InvalidRuntimeOverrideReason::CliNotFile,
             "OMO_LSP_DAEMON_CLI must point to a regular file"),
        ] {
            let env = Env::from([
                (OMO_LSP_DAEMON_CLI.to_string(), path.to_string_lossy().into_owned()),
                (OMO_LSP_DAEMON_VERSION.to_string(), "1".to_string()),
            ]);
            let error = resolve_daemon_runtime(&env, &defaults).expect_err("invalid CLI");
            assert_eq!(error, ClientRuntimeError::Override(InvalidRuntimeOverrideError {
                reason, message: message.to_string(),
            }));
        }
        let invalid = DaemonRuntime { version: "../bad".to_string(), ..defaults };
        assert!(matches!(resolve_daemon_runtime(&Env::new(), &invalid), Err(ClientRuntimeError::Version(_))));
    }
}
