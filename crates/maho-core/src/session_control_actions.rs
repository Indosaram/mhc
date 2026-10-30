//! Port of senpi packages/coding-agent/src/core/session-control-actions.ts.
//!
//! `pi.session` as one session binds it: admission and the ledger come from the session's own
//! `ExternalAdmission`, the header write from its session manager. The Rust `SessionControlActions`
//! trait (senpi-core-rest lane) has no `registerControlEndpoint` method yet; `control_endpoint_unsupported`
//! is the gate that method applies before asking the installed host.

use futures::future::BoxFuture;
use serde_json::Value;

use crate::extensions::session_control_types::SessionControlActions;

const NOT_BOUND: &str =
    "Extension runtime not initialized. pi.session cannot be used during extension loading.";

/// Why a control endpoint cannot be registered here: `unsupported_platform` on Windows, then
/// `unsupported_mode` when the running mode installed no endpoint host (only the interactive TUI does).
pub fn control_endpoint_unsupported(windows: bool, host_installed: bool) -> Option<&'static str> {
    if windows {
        return Some("unsupported_platform");
    }
    (!host_installed).then_some("unsupported_mode")
}

/// The actions `pi.session` exposes before the extension runtime is bound: every call fails.
pub struct UnboundSessionControlActions;

impl SessionControlActions for UnboundSessionControlActions {
    fn admission_gate(&self) -> Value {
        Value::Null
    }

    fn admit_external_message(&self, _input: Value) -> Result<Value, String> {
        Err(NOT_BOUND.to_owned())
    }

    fn list_admitted_deliveries(&self) -> Value {
        Value::Null
    }

    fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err("Extension runtime not initialized".to_owned()) })
    }
}

pub fn unbound_session_control_actions() -> UnboundSessionControlActions {
    UnboundSessionControlActions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_gate_checks_platform_before_mode() {
        assert_eq!(
            control_endpoint_unsupported(true, true),
            Some("unsupported_platform")
        );
        assert_eq!(
            control_endpoint_unsupported(false, false),
            Some("unsupported_mode")
        );
        assert_eq!(control_endpoint_unsupported(false, true), None);
    }

    #[tokio::test]
    async fn unbound_actions_refuse_every_call() {
        let actions = unbound_session_control_actions();
        assert_eq!(
            actions.admit_external_message(Value::Null),
            Err(NOT_BOUND.to_owned())
        );
        assert!(actions.persist_header_now().await.is_err());
        assert!(!actions.relocation_offered());
    }
}
