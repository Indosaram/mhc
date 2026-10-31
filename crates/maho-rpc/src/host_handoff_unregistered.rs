//! A handoff away from a host that no layout-2 registration proves (senpi `rpc/host-handoff-unregistered.ts`,
//! #2701).
//!
//! After an upgrade the old host is often still serving the very socket the updated client uses, and the only
//! evidence of who it is may be a flat pre-layout-2 record - or nothing provable at all. Refusing such a host with
//! `unknown_owner` left the client unable to replace it until somebody drained it by hand. An IDLE one is replaced
//! instead; one that holds work never is.
//!
//! - The sessions are counted over a connection that stays open, and counted again over that same connection once
//!   the successor owns the socket: the recount reaches the old process, and a session it accepted in between is
//!   seen there.
//! - A flat record that proves the process is the only licence to signal it, and the signal is a DRAIN (SIGUSR1),
//!   sent only when the recount still finds no session. A drain ends no work either way.
//! - Without a provable owner nothing is signalled: the old host notices that it lost the public entry and drains
//!   itself (`host_supersession.rs`). That predecessor is named in one warning line, so an old host that outlives
//!   the handoff can still be identified.
use crate::host_handoff::{HandoffContext, HandoffOptions, HandoffRefusal};
use crate::host_legacy::{busy_legacy_host_detail, describe_sessions};
use crate::host_protocol_info::HostProtocolInfo;
use crate::host_successor::RunningSuccessor;

/// One session count; an unregistered host is counted twice (before the swap and over the held connection after).
pub const SESSION_COUNT_TIMEOUT_MS: u64 = 10_000;

/// What an unregistered host's session count decides before any successor exists: `Ok` lets the handoff go on.
pub fn unregistered_refusal(
    socket: &str,
    sessions: Option<usize>,
    legacy_pid: Option<u32>,
) -> Result<(), HandoffRefusal> {
    if sessions == Some(0) {
        return Ok(());
    }
    Err(match legacy_pid {
        Some(pid) => HandoffRefusal {
            reason: "legacy_host",
            upgradeable: true,
            detail: Some(busy_legacy_host_detail(pid, socket, sessions)),
        },
        None => HandoffRefusal {
            reason: "unknown_owner",
            upgradeable: true,
            detail: Some(format!(
                "{socket} has no provable owner and holds {}; it is never signalled - let that work finish, then retry",
                describe_sessions(sessions)
            )),
        },
    })
}

pub async fn handoff_unregistered_host(
    socket: &str,
    agent_dir: &std::path::Path,
    options: HandoffOptions,
    mut context: HandoffContext,
) -> std::io::Result<Result<RunningSuccessor, HandoffRefusal>> {
    #[cfg(unix)]
    {
        let counted = crate::host_probe::hold_session_count(socket, SESSION_COUNT_TIMEOUT_MS).await;
        let legacy = crate::host_stop_drain::proven_legacy_owner(&context.paths, socket);
        if let Err(refusal) =
            unregistered_refusal(socket, counted.as_ref().map(|held| held.sessions), legacy)
        {
            return Ok(Err(refusal));
        }
        context.owner = legacy.map(|pid| crate::host_daemon_registration::RegisteredHost {
            pid,
            process_start_time: crate::host_reservations::read_process_start_time(pid),
            socket: Some(socket.to_owned()),
            instance_id: String::new(),
            generation: 0.,
            writer: None,
        });
        let drain_gate: Option<crate::host_successor::DrainGate> = counted.map(|mut held| {
            Box::pin(async move { held.recount(SESSION_COUNT_TIMEOUT_MS).await == Some(0) })
                as crate::host_successor::DrainGate
        });
        let result =
            crate::host_handoff::launch_successor(socket, agent_dir, options, &context, drain_gate)
                .await?;
        if result.is_ok() && legacy.is_none() {
            eprint!("{}", unsignalled_predecessor_warning(socket, &context.host));
        }
        Ok(result)
    }
    #[cfg(not(unix))]
    {
        let _ = (agent_dir, options, &mut context);
        Ok(Err(HandoffRefusal {
            reason: "unknown_owner",
            upgradeable: true,
            detail: Some(format!("{socket} has no provable owner")),
        }))
    }
}

/// The one record of a predecessor nobody could prove, on the caller's stderr: the endpoint's log belongs to the
/// generations writing it. Its pid is not known: no record proves it.
pub fn unsignalled_predecessor_warning(socket: &str, host: &HostProtocolInfo) -> String {
    format!(
        "generation handoff: no provable owner for {socket}; the host it replaced (instance {}, engine {}) listed 0 sessions and was not signalled; it drains itself on losing the public socket\n",
        host.instance_id.as_deref().unwrap_or("unknown"),
        host.engine_version.as_deref().unwrap_or("unknown"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_idle_host_is_replaced_and_a_busy_one_names_why() {
        assert!(unregistered_refusal("/s", Some(0), None).is_ok());
        assert!(unregistered_refusal("/s", Some(0), Some(7)).is_ok());
        let unknown = unregistered_refusal("/s", Some(2), None).unwrap_err();
        assert_eq!(unknown.reason, "unknown_owner");
        assert!(unknown.upgradeable);
        assert_eq!(
            unknown.detail.as_deref(),
            Some(
                "/s has no provable owner and holds 2 open sessions; it is never signalled - let that work finish, then retry"
            )
        );
        let silent = unregistered_refusal("/s", None, None).unwrap_err();
        assert!(
            silent
                .detail
                .unwrap()
                .contains("an unknown number of sessions")
        );
        let legacy = unregistered_refusal("/s", Some(1), Some(7)).unwrap_err();
        assert_eq!(legacy.reason, "legacy_host");
        assert!(
            legacy
                .detail
                .unwrap()
                .starts_with("pid 7 (/s) holds 1 open session;")
        );
    }

    #[test]
    fn the_warning_names_the_unsignalled_predecessor() {
        let host = crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({
            "protocolVersion": 1, "serverVersion": "1", "capabilities": [], "instanceId": "old"
        }))
        .unwrap();
        let line = unsignalled_predecessor_warning("/s", &host);
        assert!(line.contains("instance old, engine "));
        assert!(line.ends_with("it drains itself on losing the public socket\n"));
    }
}
