//! The readiness gate of a host this process just spawned (senpi `rpc/host-readiness.ts`): poll
//! `get_protocol_info` until the host answers compatibly, the budget runs out, or the spawned supervisor
//! exits. A compatible answer comes with the attach hold of the connection it arrived on.
use crate::host_attach_hold::HostAttachHold;
use crate::host_protocol_info::HostProtocolInfo;
use std::future::Future;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildExit {
    pub code: Option<i32>,
    pub signal: Option<String>,
}

pub enum ProtocolPollResult {
    Ready {
        protocol: HostProtocolInfo,
        hold: HostAttachHold,
    },
    NotReady {
        protocol: Option<HostProtocolInfo>,
        exited: Option<ChildExit>,
    },
}

const SPAWNED_HOST_PROBE_TIMEOUT_MS: u64 = 10_000;

/// `probe(socket, timeout_ms)` is one held protocol probe (production passes
/// [`crate::host_attach_hold::hold_protocol_info`]).
pub async fn poll_protocol_info<P, PFut, C, E>(
    socket: &str,
    timeout_ms: u64,
    is_compatible: C,
    child_exit: Option<E>,
    probe: P,
) -> ProtocolPollResult
where
    P: Fn(String, u64) -> PFut,
    PFut: Future<Output = Option<(HostProtocolInfo, HostAttachHold)>>,
    C: Fn(Option<&HostProtocolInfo>) -> bool,
    E: Future<Output = ChildExit>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut last_protocol: Option<HostProtocolInfo> = None;
    let mut child_exit = child_exit.map(Box::pin);
    while tokio::time::Instant::now() <= deadline {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis() as u64;
        let mut attempt = Box::pin(probe(
            socket.to_owned(),
            SPAWNED_HOST_PROBE_TIMEOUT_MS.min(remaining.max(1)),
        ));
        let (held, exited) = match child_exit.as_mut() {
            None => (attempt.await, None),
            Some(exit) => tokio::select! {
                held = &mut attempt => (held, None),
                exited = exit => {
                    // A supervisor exit can be triggered by the Windows identity watchdog while a named-pipe
                    // client is still composing its protocol reply: let this probe deliver its answer first.
                    (attempt.await, Some(exited))
                }
            },
        };
        match (held, exited) {
            (None, Some(exited)) => {
                return ProtocolPollResult::NotReady {
                    protocol: last_protocol,
                    exited: Some(exited),
                };
            }
            (Some((info, mut hold)), exited) => {
                if exited.is_some() {
                    // The exit future is spent; later rounds poll without it.
                    child_exit = None;
                }
                if is_compatible(Some(&info)) {
                    return ProtocolPollResult::Ready {
                        protocol: info,
                        hold,
                    };
                }
                last_protocol = Some(info);
                hold.release();
            }
            (None, None) => {}
        }
        tokio::time::sleep(crate::deadline::POLL_INTERVAL).await;
    }
    ProtocolPollResult::NotReady {
        protocol: last_protocol,
        exited: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_attach_hold::hold_attachment;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn info(version: f64) -> HostProtocolInfo {
        crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({
            "protocolVersion": version, "serverVersion": "v", "capabilities": []
        }))
        .expect("info")
    }

    fn held(version: f64) -> Option<(HostProtocolInfo, HostAttachHold)> {
        Some((info(version), hold_attachment(tokio::io::empty())))
    }

    #[tokio::test(start_paused = true)]
    async fn polls_until_a_compatible_answer() {
        let calls = AtomicUsize::new(0);
        let result = poll_protocol_info(
            "s",
            5_000,
            |info| info.is_some_and(|info| info.protocol_version >= 2.0),
            None::<std::future::Pending<ChildExit>>,
            |_, _| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if call == 0 {
                        None
                    } else if call == 1 {
                        held(1.0)
                    } else {
                        held(2.0)
                    }
                }
            },
        )
        .await;
        assert!(
            matches!(result, ProtocolPollResult::Ready { protocol, .. } if protocol.protocol_version == 2.0)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_exit_without_an_answer_ends_the_poll() {
        let exit = ChildExit {
            code: Some(1),
            signal: None,
        };
        let result = poll_protocol_info(
            "s",
            5_000,
            |_| true,
            Some(std::future::ready(exit.clone())),
            |_, _| async { None },
        )
        .await;
        assert!(
            matches!(result, ProtocolPollResult::NotReady { exited: Some(found), .. } if found == exit)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_incompatible_host_times_out_with_its_last_protocol() {
        let result = poll_protocol_info(
            "s",
            200,
            |_| false,
            None::<std::future::Pending<ChildExit>>,
            |_, _| async { held(1.0) },
        )
        .await;
        assert!(matches!(
            result,
            ProtocolPollResult::NotReady {
                protocol: Some(_),
                exited: None
            }
        ));
    }
}
