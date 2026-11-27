//! The launcher's side of the startup-event records (senpi#4602, A-amended section 1). A record proves
//! nothing by itself: the controller binds each producer role to its own stream before spawning, so `role`
//! here must match the stream it arrived on, and the attempt nonce must match the attempt the controller
//! allocated.

use crate::host_startup_policy::{CompleteResult, OpaqueHostResult, StartupController};
use crate::host_startup_wire::{
    MAX_STARTUP_EVENT_BYTES, STARTUP_EVENT_PROTOCOL_VERSION, StartupBinding, StartupBindingKind,
    StartupMilestone, StartupProducerRole,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_STARTUP_EVENTS_PER_PRODUCER: usize = 32;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The only role allowed to report each milestone; the controller completes the remaining phases itself.
fn milestone_producer(milestone: StartupMilestone) -> Option<StartupProducerRole> {
    match milestone {
        StartupMilestone::SupervisorBootstrap
        | StartupMilestone::SupervisorSetup
        | StartupMilestone::HostOpaque
        | StartupMilestone::InternalReady
        | StartupMilestone::ObserverReady
        | StartupMilestone::PublicPublished => Some(StartupProducerRole::Supervisor),
        StartupMilestone::HostBootstrap | StartupMilestone::HostRuntime => {
            Some(StartupProducerRole::Host)
        }
        StartupMilestone::OpaqueHost
        | StartupMilestone::PresenterResolved
        | StartupMilestone::PublicVerified => None,
    }
}

pub fn new_startup_attempt() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return uuid::Uuid::new_v4().simple().to_string();
    }
    hex::encode(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupIngressResult {
    Accepted,
    Ignored,
    Discarded,
    ProtocolFailure,
}

/// Feeds one producer stream's records into the controller. Wrong-attempt records are discarded without
/// renewing anything; duplicate sequence numbers are ignored; an unknown milestone, a record for a phase
/// another role owns, a sequence gap or an over-limit record fails the attempt.
pub struct StartupEventIngress {
    controller: Arc<StartupController>,
    binding: StartupBinding,
    role: StartupProducerRole,
    last_seq: u64,
    records: usize,
}

impl StartupEventIngress {
    pub fn new(
        controller: Arc<StartupController>,
        binding: StartupBinding,
        role: StartupProducerRole,
    ) -> Self {
        Self {
            controller,
            binding,
            role,
            last_seq: 0,
            records: 0,
        }
    }

    fn fail_protocol(&self, reason: &str) -> StartupIngressResult {
        self.controller.fail(format!(
            "RPC host startup protocol failure ({}): {reason}",
            self.role.as_str()
        ));
        StartupIngressResult::ProtocolFailure
    }

    pub fn push_line(&mut self, line: &str) -> StartupIngressResult {
        if line.len() > MAX_STARTUP_EVENT_BYTES {
            return self.fail_protocol("record exceeds 4096 bytes");
        }
        let Some(record) = parse_record(line) else {
            return self.fail_protocol("malformed record");
        };
        if !same_binding(&record.binding, &self.binding) {
            return StartupIngressResult::Discarded;
        }
        let Some(record_role) = record.role.as_str() else {
            return self.fail_protocol(&format!("record claims role {}", record.role));
        };
        if record_role != self.role.as_str() {
            return self.fail_protocol(&format!("record claims role {record_role}"));
        }
        if record.seq <= self.last_seq {
            return StartupIngressResult::Ignored;
        }
        if record.seq != self.last_seq + 1 {
            return self.fail_protocol(&format!("sequence gap after {}", self.last_seq));
        }
        self.records += 1;
        if self.records > MAX_STARTUP_EVENTS_PER_PRODUCER {
            return self.fail_protocol("too many records");
        }
        self.last_seq = record.seq;
        let Some(milestone_str) = record.milestone.as_str() else {
            return self.fail_protocol(&format!("unknown milestone {}", record.milestone));
        };
        let Some(milestone) = StartupMilestone::parse(milestone_str) else {
            return self.fail_protocol(&format!("unknown milestone {milestone_str}"));
        };
        if milestone_producer(milestone) != Some(self.role) {
            return self.fail_protocol(&format!(
                "{} cannot report {milestone_str}",
                self.role.as_str()
            ));
        }
        let accepted = match milestone {
            StartupMilestone::HostOpaque => match self.controller.declare_opaque_host() {
                OpaqueHostResult::Accepted => CompleteResult::Accepted,
                OpaqueHostResult::Rejected => CompleteResult::Rejected,
            },
            other => match other.into_phase() {
                Some(phase) => self.controller.complete(phase),
                None => CompleteResult::Rejected,
            },
        };
        match accepted {
            CompleteResult::Accepted => StartupIngressResult::Accepted,
            CompleteResult::Ignored => StartupIngressResult::Ignored,
            CompleteResult::Rejected => {
                if self.controller.terminal().is_some() {
                    StartupIngressResult::Ignored
                } else {
                    self.fail_protocol(&format!(
                        "{milestone_str} reported before its phase was granted"
                    ))
                }
            }
        }
    }
}

/// Reads the supervisor's startup pipe: its own records go to the supervisor ingress, relayed host-child
/// records to the host ingress. A line that outgrows a relay frame of the record limit fails the attempt.
pub fn attach_startup_events<R>(
    mut stream: R,
    controller: Arc<StartupController>,
    binding: StartupBinding,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut supervisor = StartupEventIngress::new(
        controller.clone(),
        binding.clone(),
        StartupProducerRole::Supervisor,
    );
    let mut host = StartupEventIngress::new(controller.clone(), binding, StartupProducerRole::Host);
    let max_line = 2 * MAX_STARTUP_EVENT_BYTES + 64;
    tokio::spawn(async move {
        let mut pending = String::new();
        let mut buf = [0u8; 4096];
        while let Ok(count) = stream.read(&mut buf).await {
            if count == 0 {
                break;
            }
            if controller.terminal().is_some() {
                continue;
            }
            pending.push_str(&String::from_utf8_lossy(&buf[..count]));
            while let Some(newline) = pending.find('\n') {
                let line = pending[..newline].to_owned();
                pending.drain(..=newline);
                if let Some(relayed) = unwrap_startup_relay(&line) {
                    host.push_line(&relayed);
                } else {
                    supervisor.push_line(&line);
                }
                if controller.terminal().is_some() {
                    break;
                }
            }
            if controller.terminal().is_none() && pending.len() > max_line {
                supervisor.push_line(&pending);
            }
        }
    })
}

/// The host-child record a relay frame carries, or `None` for the supervisor's own records.
pub fn unwrap_startup_relay(line: &str) -> Option<String> {
    if !line.starts_with("{\"relay\":") {
        return None;
    }
    let frame: Value = serde_json::from_str(line).ok()?;
    let obj = frame.as_object()?;
    let relay = obj.get("relay")?.as_str()?;
    let inner = obj.get("line")?.as_str()?;
    (relay == "host").then(|| inner.to_owned())
}

struct RawRecord {
    binding: Value,
    role: Value,
    seq: u64,
    milestone: Value,
}

fn parse_record(line: &str) -> Option<RawRecord> {
    let value: Value = serde_json::from_str(line).ok()?;
    let record = value.as_object()?;
    if record.get("v")?.as_u64()? != u64::from(STARTUP_EVENT_PROTOCOL_VERSION) {
        return None;
    }
    let seq = record.get("seq")?.as_u64()?;
    if !(1..=MAX_SAFE_INTEGER).contains(&seq) {
        return None;
    }
    Some(RawRecord {
        binding: record.get("binding")?.clone(),
        role: record.get("role")?.clone(),
        seq,
        milestone: record.get("milestone")?.clone(),
    })
}

fn same_binding(candidate: &Value, binding: &StartupBinding) -> bool {
    let Some(other) = candidate.as_object() else {
        return false;
    };
    let launch_kind_str = match binding.launch_kind {
        StartupBindingKind::Initial => "initial",
        StartupBindingKind::Successor => "successor",
    };
    let predecessor_matches = match (&binding.predecessor, other.get("predecessor")) {
        (None, Some(Value::Null)) => true,
        (Some(expected), Some(Value::String(actual))) => expected == actual,
        _ => false,
    };
    other.get("attempt").and_then(Value::as_str) == Some(binding.attempt.as_str())
        && other.get("endpoint").and_then(Value::as_str) == Some(binding.endpoint.as_str())
        && other.get("instanceId").and_then(Value::as_str) == Some(binding.instance_id.as_str())
        && other.get("generation").and_then(Value::as_u64) == Some(binding.generation)
        && other.get("launchKind").and_then(Value::as_str) == Some(launch_kind_str)
        && predecessor_matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_startup_policy::tests::VirtualClock;
    use crate::host_startup_policy::{StartupLaunchKind, StartupOutcome, start_startup_controller};
    use crate::host_startup_wire::{relay_startup_line, startup_event_line};
    use tokio::io::AsyncWriteExt;

    fn binding() -> StartupBinding {
        StartupBinding {
            attempt: new_startup_attempt(),
            endpoint: "/tmp/rpc.sock".into(),
            instance_id: "gen-1".into(),
            generation: 1,
            launch_kind: StartupBindingKind::Initial,
            predecessor: None,
        }
    }

    #[test]
    fn ingress_validates_bindings_roles_sequence_and_phase_order() {
        let clock = VirtualClock::new();
        let ctrl = start_startup_controller(StartupLaunchKind::Initial, clock, None);
        let b = binding();
        let mut sup =
            StartupEventIngress::new(ctrl.clone(), b.clone(), StartupProducerRole::Supervisor);
        let mut host = StartupEventIngress::new(ctrl.clone(), b.clone(), StartupProducerRole::Host);

        // Wrong binding is discarded without failing.
        let mut other = b.clone();
        other.attempt = "other".into();
        let wrong_line = startup_event_line(
            &other,
            StartupProducerRole::Supervisor,
            1,
            StartupMilestone::SupervisorBootstrap,
        );
        assert_eq!(
            sup.push_line(wrong_line.trim_end()),
            StartupIngressResult::Discarded
        );
        assert!(ctrl.terminal().is_none());

        // Supervisor bootstrap accepted; duplicate seq ignored.
        let line1 = startup_event_line(
            &b,
            StartupProducerRole::Supervisor,
            1,
            StartupMilestone::SupervisorBootstrap,
        );
        assert_eq!(
            sup.push_line(line1.trim_end()),
            StartupIngressResult::Accepted
        );
        assert_eq!(
            sup.push_line(line1.trim_end()),
            StartupIngressResult::Ignored
        );

        // Host cannot report supervisor milestone.
        let bad_role = startup_event_line(
            &b,
            StartupProducerRole::Host,
            1,
            StartupMilestone::SupervisorSetup,
        );
        assert_eq!(
            host.push_line(bad_role.trim_end()),
            StartupIngressResult::ProtocolFailure
        );
        assert!(matches!(
            ctrl.terminal(),
            Some(StartupOutcome::Failed { .. })
        ));
    }

    #[test]
    fn ingress_rejects_sequence_gaps_and_oversized_records() {
        let clock = VirtualClock::new();
        let ctrl = start_startup_controller(StartupLaunchKind::Initial, clock, None);
        let b = binding();
        let mut sup =
            StartupEventIngress::new(ctrl.clone(), b.clone(), StartupProducerRole::Supervisor);
        let line2 = startup_event_line(
            &b,
            StartupProducerRole::Supervisor,
            2,
            StartupMilestone::SupervisorBootstrap,
        );
        assert_eq!(
            sup.push_line(line2.trim_end()),
            StartupIngressResult::ProtocolFailure
        );

        let clock2 = VirtualClock::new();
        let ctrl2 = start_startup_controller(StartupLaunchKind::Initial, clock2, None);
        let mut sup2 = StartupEventIngress::new(ctrl2, b, StartupProducerRole::Supervisor);
        let huge = "x".repeat(MAX_STARTUP_EVENT_BYTES + 1);
        assert_eq!(sup2.push_line(&huge), StartupIngressResult::ProtocolFailure);
    }

    #[tokio::test]
    async fn attach_startup_events_routes_supervisor_and_relayed_host_records() {
        let clock = VirtualClock::new();
        let ctrl = start_startup_controller(StartupLaunchKind::Initial, clock, None);
        let b = binding();
        let (mut writer, reader) = tokio::io::duplex(8192);
        let task = attach_startup_events(reader, ctrl.clone(), b.clone());

        writer
            .write_all(
                startup_event_line(
                    &b,
                    StartupProducerRole::Supervisor,
                    1,
                    StartupMilestone::SupervisorBootstrap,
                )
                .as_bytes(),
            )
            .await
            .expect("write 1");
        writer
            .write_all(
                startup_event_line(
                    &b,
                    StartupProducerRole::Supervisor,
                    2,
                    StartupMilestone::SupervisorSetup,
                )
                .as_bytes(),
            )
            .await
            .expect("write 2");
        let host_line = startup_event_line(
            &b,
            StartupProducerRole::Host,
            1,
            StartupMilestone::HostBootstrap,
        );
        writer
            .write_all(relay_startup_line(host_line.trim_end()).as_bytes())
            .await
            .expect("write relay");
        drop(writer);
        task.await.expect("join");
        assert_eq!(
            ctrl.phase(),
            crate::host_startup_policy::StartupPhase::HostRuntime
        );
    }
}
