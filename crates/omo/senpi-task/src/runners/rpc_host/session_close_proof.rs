//! `runners/rpc-host/session-close-proof.ts`: a close acknowledgement may only detach our
//! attachment; the host's live session table owns absence.

use super::host_command_dispatch::HostRpcClient;
use super::session_wire::host_session_unresponsive_error;
use crate::host::HostError;

/// `HostSessionDetachedError("confirm close_session")`.
fn detached() -> HostError {
    HostError {
        message: "the host session holds no connection (confirm close_session)".to_string(),
    }
}

/// `confirmHostSessionAbsent`: send `close_session`, then list the host's live sessions
/// (`include_workers: true`) and succeed only when `routing_id` is absent. Even a refused or lost
/// acknowledgement may follow an actual close, so the listing runs AFTER that attempt.
pub fn confirm_host_session_absent(
    client: &dyn HostRpcClient,
    routing_id: &str,
    connected: &dyn Fn() -> bool,
) -> Result<(), HostError> {
    let close_error = client.close(routing_id).err();
    if !connected() {
        return Err(detached());
    }
    let Some(sessions) = client.list_sessions()? else {
        return Err(close_error.unwrap_or_else(host_session_unresponsive_error));
    };
    if !connected() {
        return Err(detached());
    }
    if sessions.iter().any(|session| session == routing_id) {
        return Err(close_error.unwrap_or_else(host_session_unresponsive_error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::Value;

    use super::*;
    use crate::manager::child_handle::{ManagedChildListener, Unsubscribe};

    struct Host {
        close_fails: bool,
        listing: Option<Vec<String>>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl HostRpcClient for Host {
        fn command(&self, _command: Value) -> Result<(), HostError> {
            Ok(())
        }
        fn on_event(&self, _listener: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn close(&self, _routing_id: &str) -> Result<(), HostError> {
            self.calls.lock().expect("calls").push("close_session");
            if self.close_fails {
                return Err(HostError {
                    message: "close refused".into(),
                });
            }
            Ok(())
        }
        fn list_sessions(&self) -> Result<Option<Vec<String>>, HostError> {
            self.calls.lock().expect("calls").push("list_sessions");
            Ok(self.listing.clone())
        }
    }

    fn host(close_fails: bool, listing: Option<&[&str]>) -> Host {
        Host {
            close_fails,
            listing: listing.map(|ids| ids.iter().map(|id| id.to_string()).collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// `session-confirmed-close.test.ts`: only the post-close listing releases ownership.
    #[test]
    fn absence_is_proven_by_the_listing_not_the_ack() {
        let gone = host(true, Some(&["other"]));
        assert!(confirm_host_session_absent(&gone, "route", &|| true).is_ok());
        assert_eq!(
            *gone.calls.lock().expect("calls"),
            vec!["close_session", "list_sessions"]
        );
        let still_listed = host(false, Some(&["route"]));
        let error =
            confirm_host_session_absent(&still_listed, "route", &|| true).expect_err("still live");
        assert!(crate::runners::rpc_host::session_wire::is_host_session_unresponsive(&error));
        let refused_and_listed = host(true, Some(&["route"]));
        assert_eq!(
            confirm_host_session_absent(&refused_and_listed, "route", &|| true)
                .expect_err("still live")
                .message,
            "close refused"
        );
    }

    #[test]
    fn an_engine_without_the_listing_cannot_prove_absence() {
        let blind = host(false, None);
        assert!(confirm_host_session_absent(&blind, "route", &|| true).is_err());
        let detached_host = host(false, Some(&[]));
        assert!(confirm_host_session_absent(&detached_host, "route", &|| false).is_err());
    }
}
