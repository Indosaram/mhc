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

    /// A host whose `close_session` answers are scripted per call; the listing reports `route`
    /// while it is still open. Every close names the routing id it was asked to close.
    struct ScriptedHost {
        closes: Mutex<std::collections::VecDeque<Result<(), HostError>>>,
        open: Mutex<bool>,
        closed_routes: Mutex<Vec<String>>,
        listings: Mutex<usize>,
    }

    impl ScriptedHost {
        fn new(closes: Vec<Result<(), HostError>>) -> Self {
            Self {
                closes: Mutex::new(closes.into()),
                open: Mutex::new(true),
                closed_routes: Mutex::new(Vec::new()),
                listings: Mutex::new(0),
            }
        }
    }

    impl HostRpcClient for ScriptedHost {
        fn command(&self, _command: Value) -> Result<(), HostError> {
            Ok(())
        }
        fn on_event(&self, _listener: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn close(&self, routing_id: &str) -> Result<(), HostError> {
            self.closed_routes
                .lock()
                .expect("routes")
                .push(routing_id.to_string());
            let answer = self
                .closes
                .lock()
                .expect("closes")
                .pop_front()
                .unwrap_or(Ok(()));
            if answer.is_ok() {
                *self.open.lock().expect("open") = false;
            }
            answer
        }
        fn list_sessions(&self) -> Result<Option<Vec<String>>, HostError> {
            *self.listings.lock().expect("listings") += 1;
            Ok(Some(if *self.open.lock().expect("open") {
                vec!["route".to_string()]
            } else {
                Vec::new()
            }))
        }
    }

    /// A host whose connection is cut while `close_session` is withheld: the engine swallows the
    /// transport error (the close "succeeds"), and every later request fails as transport-gone.
    /// The session itself is still live on the host.
    struct CutHost {
        cut: Mutex<bool>,
        sessions: Mutex<Vec<String>>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl HostRpcClient for CutHost {
        fn command(&self, _command: Value) -> Result<(), HostError> {
            Ok(())
        }
        fn on_event(&self, _listener: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn close(&self, _routing_id: &str) -> Result<(), HostError> {
            self.calls.lock().expect("calls").push("close_session");
            *self.cut.lock().expect("cut") = true;
            Ok(())
        }
        fn list_sessions(&self) -> Result<Option<Vec<String>>, HostError> {
            self.calls.lock().expect("calls").push("list_sessions");
            if *self.cut.lock().expect("cut") {
                return Err(HostError {
                    message: "RPC transport is gone".into(),
                });
            }
            Ok(Some(self.sessions.lock().expect("sessions").clone()))
        }
    }

    /// `session-confirmed-close.test.ts` "a transport cut swallowed by the engine close never
    /// proves a retained session ended", at the `HostRpcClient` seam: the swallowed close is not
    /// proof, and neither the dead listing nor a detached connection confirms absence.
    #[test]
    fn a_transport_cut_swallowed_by_the_engine_close_never_proves_a_retained_session_ended() {
        let host = CutHost {
            cut: Mutex::new(false),
            sessions: Mutex::new(vec!["route".to_string()]),
            calls: Mutex::new(Vec::new()),
        };
        let cut = || !*host.cut.lock().expect("cut");
        let unconfirmed = confirm_host_session_absent(&host, "route", &|| true)
            .expect_err("a cut transport cannot list the session away");
        assert_eq!(unconfirmed.message, "RPC transport is gone");
        assert_eq!(
            *host.calls.lock().expect("calls"),
            vec!["close_session", "list_sessions"]
        );
        *host.cut.lock().expect("cut") = false;
        host.calls.lock().expect("calls").clear();
        let gone = confirm_host_session_absent(&host, "route", &cut)
            .expect_err("the connection that closed is gone");
        assert_eq!(gone.message, detached().message);
        assert_eq!(*host.calls.lock().expect("calls"), vec!["close_session"]);
        assert_eq!(*host.sessions.lock().expect("sessions"), vec!["route"]);
    }

    /// `session-confirmed-close.test.ts`: a refused close leaves the same route open, and a retry
    /// on that route is confirmed once the host no longer lists it.
    #[test]
    fn a_refused_confirmed_close_keeps_the_same_session_route_available_for_retry() {
        let host = ScriptedHost::new(vec![Err(HostError {
            message: "host_draining".into(),
        })]);
        let refused =
            confirm_host_session_absent(&host, "route", &|| true).expect_err("close refused");
        assert_eq!(refused.message, "host_draining");
        assert!(*host.open.lock().expect("open"));

        confirm_host_session_absent(&host, "route", &|| true).expect("retry confirmed");
        assert_eq!(
            *host.closed_routes.lock().expect("routes"),
            vec!["route".to_string(), "route".to_string()]
        );
        assert_eq!(*host.listings.lock().expect("listings"), 2);
    }
}
