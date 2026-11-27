//! Drives one start attempt from its controller to READY or a terminal failure (senpi#4602, A-amended):
//! the producer milestones advance the phases on their own; this module completes the two phases the
//! launcher owns - the presenter result and the public `get_protocol_info` verification - each inside
//! its phase's remaining time, and turns a child exit or an incompatible answer into an explicit,
//! latched failure.

use crate::host_attach_hold::HostAttachHold;
use crate::host_decision::{HOST_PROTOCOL_VERSION, REQUIRED_HOST_CAPABILITIES};
use crate::host_protocol_info::{HostLifecycle, HostProtocolInfo};
use crate::host_readiness::{ChildExit, ProtocolPollResult, poll_protocol_info};
use crate::host_startup_beacon::HOST_STARTUP_EVENTS_ENV;
use crate::host_startup_events::attach_startup_events;
use crate::host_startup_policy::{
    CompleteResult, STARTUP_DECISION_BUDGET_MS, StartupClock, StartupController, StartupLaunchKind,
    StartupOutcome, StartupPhase, StartupTimeout, StartupTimerHandle, start_startup_controller,
};
use crate::host_startup_wire::{
    STARTUP_EVENT_FD, StartupBinding, StartupBindingKind, StartupHandOff, StartupProducerRole,
    encode_startup_hand_off,
};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncRead;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFailure {
    Timeout(StartupTimeout),
    Failed {
        phase: StartupPhase,
        cause: String,
        elapsed_ms: u64,
    },
}

impl StartupFailure {
    pub fn from_outcome(outcome: StartupOutcome) -> Result<Self, StartupOutcome> {
        match outcome {
            StartupOutcome::Ready { .. } => Err(outcome),
            StartupOutcome::Timeout(timeout) => Ok(Self::Timeout(timeout)),
            StartupOutcome::Failed {
                phase,
                cause,
                elapsed_ms,
            } => Ok(Self::Failed {
                phase,
                cause,
                elapsed_ms,
            }),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Timeout(t) => t.elapsed_ms,
            Self::Failed { elapsed_ms, .. } => *elapsed_ms,
        }
    }
}

/// The launcher's monotonic clock: never the wall clock, never another process's timestamps.
pub struct MonotonicStartupClock {
    origin: Instant,
}

impl MonotonicStartupClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicStartupClock {
    fn default() -> Self {
        Self::new()
    }
}

impl StartupClock for MonotonicStartupClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    fn schedule_at(&self, at_ms: u64, fire: Box<dyn FnOnce() + Send>) -> StartupTimerHandle {
        let now = self.now_ms();
        let delay = Duration::from_millis(at_ms.saturating_sub(now));
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return StartupTimerHandle::noop();
        };
        let task = handle.spawn(async move {
            tokio::time::sleep(delay).await;
            fire();
        });
        StartupTimerHandle::new(move || task.abort())
    }
}

pub fn monotonic_startup_clock() -> Arc<dyn StartupClock> {
    Arc::new(MonotonicStartupClock::new())
}

pub struct StartupAttempt {
    pub controller: Arc<StartupController>,
    pub binding: StartupBinding,
    pub coordinated: bool,
    budget_ms: u64,
    fillers: Vec<i32>,
}

impl StartupAttempt {
    /// The supervisor's hand-off variable, for a coordinated launch; read it after `prepare_fillers`,
    /// which decides the fillers.
    pub fn env(&self) -> HashMap<String, String> {
        if !self.coordinated {
            return HashMap::new();
        }
        let encoded = encode_startup_hand_off(&StartupHandOff {
            binding: self.binding.clone(),
            role: StartupProducerRole::Supervisor,
            budget_ms: self.budget_ms,
            fillers: self.fillers.clone(),
        });
        HashMap::from([(HOST_STARTUP_EVENTS_ENV.to_owned(), encoded)])
    }

    /// Computes the filler slots `3..STARTUP_EVENT_FD` for a coordinated launch given the slots
    /// already occupied by real inherited descriptors (such as fd 3 owner watch, fd 4 presenter).
    pub fn prepare_fillers(&mut self, occupied_slots: &[i32]) -> &[i32] {
        self.fillers.clear();
        if self.coordinated {
            for slot in 3..STARTUP_EVENT_FD {
                if !occupied_slots.contains(&slot) {
                    self.fillers.push(slot);
                }
            }
        }
        &self.fillers
    }

    pub fn attach<R, E>(&self, startup_stream: Option<R>, child_exit: E)
    where
        R: AsyncRead + Unpin + Send + 'static,
        E: Future<Output = ChildExit> + Send + 'static,
    {
        if self.coordinated
            && let Some(stream) = startup_stream
        {
            attach_startup_events(stream, self.controller.clone(), self.binding.clone());
        }
        let controller = self.controller.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let exit = child_exit.await;
                if controller.terminal().is_none()
                    && controller.phase() != StartupPhase::PublicVerified
                {
                    controller.fail(exit_message(&exit));
                }
            });
        }
    }
}

pub struct BeginStartupAttemptOptions {
    pub socket: String,
    pub instance_id: String,
    pub generation: u64,
    pub coordinated: bool,
    pub decision_budget_ms: Option<u64>,
    pub clock: Option<Arc<dyn StartupClock>>,
    pub attempt: String,
    pub successor_of: Option<Option<String>>,
}

/// Created immediately BEFORE the spawn: the controller's creation instant is the attempt's origin.
pub fn begin_startup_attempt(options: BeginStartupAttemptOptions) -> StartupAttempt {
    let successor = options.successor_of.is_some();
    let launch_kind = if !options.coordinated {
        StartupLaunchKind::Legacy
    } else if successor {
        StartupLaunchKind::Successor
    } else {
        StartupLaunchKind::Initial
    };
    let budget_ms = options
        .decision_budget_ms
        .unwrap_or(STARTUP_DECISION_BUDGET_MS);
    let binding = StartupBinding {
        attempt: options.attempt,
        endpoint: options.socket,
        instance_id: options.instance_id,
        generation: options.generation,
        launch_kind: if successor {
            StartupBindingKind::Successor
        } else {
            StartupBindingKind::Initial
        },
        predecessor: options.successor_of.flatten(),
    };
    let clock = options.clock.unwrap_or_else(monotonic_startup_clock);
    let controller = start_startup_controller(launch_kind, clock, Some(budget_ms));
    StartupAttempt {
        controller,
        binding,
        coordinated: options.coordinated,
        budget_ms,
        fillers: Vec::new(),
    }
}

pub fn exit_message(exit: &ChildExit) -> String {
    format!(
        "RPC socket host exited with {} before answering get_protocol_info",
        exit_status(exit)
    )
}

fn exit_status(exit: &ChildExit) -> String {
    let code = exit
        .code
        .map_or_else(|| "null".to_owned(), |c| c.to_string());
    let signal = exit
        .signal
        .as_deref()
        .map_or_else(String::new, |sig| format!(" ({sig})"));
    format!("code {code}{signal}")
}

/// The reply a host gave instead of READY. A stopping host is not a protocol mismatch (senpi#6017).
fn answer_message(protocol: &HostProtocolInfo, exited: Option<&ChildExit>) -> String {
    if protocol.lifecycle != Some(HostLifecycle::Stopping) {
        return incompatible_message(protocol);
    }
    let exit = exited.map_or_else(String::new, |ex| {
        format!(", and then exited with {}", exit_status(ex))
    });
    format!(
        "RPC socket host answered get_protocol_info with lifecycle \"stopping\": it began stopping before this start verified it{exit}"
    )
}

fn incompatible_message(protocol: &HostProtocolInfo) -> String {
    let caps = serde_json::to_string(&protocol.capabilities).unwrap_or_else(|_| "[]".into());
    let req = serde_json::to_string(&REQUIRED_HOST_CAPABILITIES).unwrap_or_else(|_| "[]".into());
    format!(
        "RPC socket host answered get_protocol_info with protocolVersion {}, serverVersion {} and capabilities {caps}, but is incompatible with protocol version {HOST_PROTOCOL_VERSION} and required capabilities {req}",
        protocol.protocol_version, protocol.server_version
    )
}

pub enum StartupResult {
    Ready { hold: HostAttachHold },
    NotReady { outcome: StartupFailure },
}

pub async fn drive_startup_to_ready<P, PFut, C, E>(
    attempt: &StartupAttempt,
    socket: &str,
    child_exit: E,
    presenter_enrollment: Option<BoxFuture<'static, ()>>,
    is_compatible: C,
    probe: P,
) -> StartupResult
where
    P: Fn(String, u64) -> PFut,
    PFut: Future<Output = Option<(HostProtocolInfo, HostAttachHold)>>,
    C: Fn(Option<&HostProtocolInfo>) -> bool,
    E: Future<Output = ChildExit>,
{
    let controller = &attempt.controller;
    let failure = || async {
        let settled = controller.settled().await;
        match StartupFailure::from_outcome(settled) {
            Ok(outcome) => StartupResult::NotReady { outcome },
            Err(_) => StartupResult::NotReady {
                outcome: StartupFailure::Failed {
                    phase: controller.phase(),
                    cause: "startup controller reported READY on a failure path".to_owned(),
                    elapsed_ms: 0,
                },
            },
        }
    };

    let mut child_exit_pin = Box::pin(child_exit);
    let mut presenter_enrollment = presenter_enrollment;

    if attempt.coordinated {
        if !controller.when_phase(StartupPhase::PresenterResolved).await {
            return failure().await;
        }
        let presenter = presenter_enrollment
            .take()
            .unwrap_or_else(|| Box::pin(async {}));
        let resolved = tokio::select! {
            biased;
            _ = controller.settled() => false,
            _ = presenter => true,
        };
        if !resolved
            || controller.complete(StartupPhase::PresenterResolved) != CompleteResult::Accepted
        {
            return failure().await;
        }
    }

    let timeout_ms = controller.remaining_ms().max(1);
    let result = poll_protocol_info(
        socket,
        timeout_ms,
        is_compatible,
        Some(&mut child_exit_pin),
        probe,
    )
    .await;

    match result {
        ProtocolPollResult::Ready { mut hold, .. } => {
            if controller.complete(StartupPhase::PublicVerified) != CompleteResult::Accepted {
                hold.release();
                return failure().await;
            }
            if !attempt.coordinated
                && let Some(enrollment) = presenter_enrollment
            {
                enrollment.await;
            }
            StartupResult::Ready { hold }
        }
        ProtocolPollResult::NotReady { protocol, exited } => {
            let explicit = if let Some(info) = protocol {
                Some(answer_message(&info, exited.as_ref()))
            } else {
                exited.as_ref().map(exit_message)
            };
            let Some(cause) = explicit else {
                return failure().await;
            };
            controller.fail(cause.clone());
            let settled = controller.settled().await;
            let outcome = match StartupFailure::from_outcome(settled) {
                Ok(StartupFailure::Failed {
                    phase,
                    cause: latched,
                    elapsed_ms,
                }) => StartupFailure::Failed {
                    phase,
                    cause: latched,
                    elapsed_ms,
                },
                Ok(other) => StartupFailure::Failed {
                    phase: StartupPhase::PublicVerified,
                    cause,
                    elapsed_ms: other.elapsed_ms(),
                },
                Err(StartupOutcome::Ready { elapsed_ms })
                | Err(StartupOutcome::Failed { elapsed_ms, .. }) => StartupFailure::Failed {
                    phase: StartupPhase::PublicVerified,
                    cause,
                    elapsed_ms,
                },
                Err(StartupOutcome::Timeout(timeout)) => StartupFailure::Failed {
                    phase: StartupPhase::PublicVerified,
                    cause,
                    elapsed_ms: timeout.elapsed_ms,
                },
            };
            StartupResult::NotReady { outcome }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_attach_hold::hold_attachment;
    use crate::host_startup_policy::tests::VirtualClock;
    use serde_json::json;

    fn info(version: f64, lifecycle: Option<&str>) -> HostProtocolInfo {
        let mut raw = json!({
            "protocolVersion": version,
            "serverVersion": "1",
            "capabilities": REQUIRED_HOST_CAPABILITIES,
        });
        if let Some(lc) = lifecycle {
            raw["lifecycle"] = json!(lc);
        }
        crate::host_protocol_info::parse_host_protocol_info(&raw).expect("info")
    }

    #[tokio::test]
    async fn coordinated_attempt_drives_to_ready_with_hold() {
        let clock = VirtualClock::new();
        let mut attempt = begin_startup_attempt(BeginStartupAttemptOptions {
            socket: "/tmp/s.sock".into(),
            instance_id: "g1".into(),
            generation: 1,
            coordinated: true,
            decision_budget_ms: None,
            clock: Some(clock),
            attempt: "att-1".into(),
            successor_of: None,
        });
        assert_eq!(attempt.prepare_fillers(&[3, 4]).len(), 11);
        assert!(attempt.env().contains_key(HOST_STARTUP_EVENTS_ENV));

        for phase in [
            StartupPhase::SupervisorBootstrap,
            StartupPhase::SupervisorSetup,
            StartupPhase::HostBootstrap,
            StartupPhase::HostRuntime,
            StartupPhase::InternalReady,
            StartupPhase::ObserverReady,
            StartupPhase::PublicPublished,
        ] {
            assert_eq!(attempt.controller.complete(phase), CompleteResult::Accepted);
        }

        let res = drive_startup_to_ready(
            &attempt,
            "/tmp/s.sock",
            std::future::pending::<ChildExit>(),
            Some(Box::pin(async {})),
            |p| p.is_some_and(|h| h.protocol_version == 1.0),
            |_, _| async { Some((info(1.0, None), hold_attachment(tokio::io::empty()))) },
        )
        .await;
        assert!(matches!(res, StartupResult::Ready { .. }));
        assert!(matches!(
            attempt.controller.terminal(),
            Some(StartupOutcome::Ready { .. })
        ));
    }

    #[tokio::test]
    async fn stopping_answer_reports_explicit_cause_not_protocol_mismatch() {
        let clock = VirtualClock::new();
        let attempt = begin_startup_attempt(BeginStartupAttemptOptions {
            socket: "/tmp/s.sock".into(),
            instance_id: "g1".into(),
            generation: 1,
            coordinated: false,
            decision_budget_ms: Some(10),
            clock: Some(clock),
            attempt: "att-1".into(),
            successor_of: None,
        });
        let res = drive_startup_to_ready(
            &attempt,
            "/tmp/s.sock",
            std::future::ready(ChildExit {
                code: Some(0),
                signal: None,
            }),
            None,
            |_| false,
            |_, _| async {
                Some((
                    info(1.0, Some("stopping")),
                    hold_attachment(tokio::io::empty()),
                ))
            },
        )
        .await;
        match res {
            StartupResult::NotReady {
                outcome: StartupFailure::Failed { cause, .. },
            } => {
                assert!(cause.contains("lifecycle \"stopping\""));
                assert!(cause.contains("code 0"));
            }
            _ => panic!("expected explicit stopping failure"),
        }
    }
}
