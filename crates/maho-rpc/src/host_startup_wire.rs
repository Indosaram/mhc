//! The startup-event wire (senpi#4602, A-amended sections 1-2), shared by the producers (the supervisor
//! and its host child) and the launcher's controller.
//!
//! Each producer writes LF-terminated JSON records to the dedicated startup pipe at `STARTUP_EVENT_FD`.
//! The supervisor relays its host child's records inside a `relay` frame, so the launcher feeds them to
//! the host role's ingress and a supervisor record can never complete a host phase.

use crate::host_startup_policy::StartupPhase;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Above `ADOPTABLE_MAX` of the private descriptor table (`private-fds.ts`), which forwards 5..15 by number.
pub const STARTUP_EVENT_FD: i32 = 16;
pub const STARTUP_EVENT_PROTOCOL_VERSION: u32 = 1;
pub const MAX_STARTUP_EVENT_BYTES: usize = 4096;
/// In the candidate's own generation directory: its attempt nonce and identity, for a later holder's recovery.
pub const STARTUP_ATTEMPT_FILE: &str = "startup-attempt.json";
/// In a successor's own generation directory: the launcher's grant to take the public name, for one attempt.
pub const ADOPTION_GRANT_FILE: &str = "adoption-grant.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupProducerRole {
    Supervisor,
    Host,
}

impl StartupProducerRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supervisor => "supervisor",
            Self::Host => "host",
        }
    }
}

/// `HostOpaque`: the supervisor's declaration, made before it spawns a host child that cannot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupMilestone {
    SupervisorBootstrap,
    SupervisorSetup,
    HostBootstrap,
    HostRuntime,
    OpaqueHost,
    InternalReady,
    ObserverReady,
    PublicPublished,
    PresenterResolved,
    PublicVerified,
    HostOpaque,
}

impl StartupMilestone {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SupervisorBootstrap => "supervisor_bootstrap",
            Self::SupervisorSetup => "supervisor_setup",
            Self::HostBootstrap => "host_bootstrap",
            Self::HostRuntime => "host_runtime",
            Self::OpaqueHost => "opaque_host",
            Self::InternalReady => "internal_ready",
            Self::ObserverReady => "observer_ready",
            Self::PublicPublished => "public_published",
            Self::PresenterResolved => "presenter_resolved",
            Self::PublicVerified => "public_verified",
            Self::HostOpaque => "host_opaque",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "host_opaque" => Some(Self::HostOpaque),
            other => StartupPhase::parse(other).map(Self::from_phase),
        }
    }

    pub const fn from_phase(phase: StartupPhase) -> Self {
        match phase {
            StartupPhase::SupervisorBootstrap => Self::SupervisorBootstrap,
            StartupPhase::SupervisorSetup => Self::SupervisorSetup,
            StartupPhase::HostBootstrap => Self::HostBootstrap,
            StartupPhase::HostRuntime => Self::HostRuntime,
            StartupPhase::OpaqueHost => Self::OpaqueHost,
            StartupPhase::InternalReady => Self::InternalReady,
            StartupPhase::ObserverReady => Self::ObserverReady,
            StartupPhase::PublicPublished => Self::PublicPublished,
            StartupPhase::PresenterResolved => Self::PresenterResolved,
            StartupPhase::PublicVerified => Self::PublicVerified,
        }
    }

    pub const fn into_phase(self) -> Option<StartupPhase> {
        match self {
            Self::SupervisorBootstrap => Some(StartupPhase::SupervisorBootstrap),
            Self::SupervisorSetup => Some(StartupPhase::SupervisorSetup),
            Self::HostBootstrap => Some(StartupPhase::HostBootstrap),
            Self::HostRuntime => Some(StartupPhase::HostRuntime),
            Self::OpaqueHost => Some(StartupPhase::OpaqueHost),
            Self::InternalReady => Some(StartupPhase::InternalReady),
            Self::ObserverReady => Some(StartupPhase::ObserverReady),
            Self::PublicPublished => Some(StartupPhase::PublicPublished),
            Self::PresenterResolved => Some(StartupPhase::PresenterResolved),
            Self::PublicVerified => Some(StartupPhase::PublicVerified),
            Self::HostOpaque => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupBindingKind {
    Initial,
    Successor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupBinding {
    pub attempt: String,
    pub endpoint: String,
    pub instance_id: String,
    pub generation: u64,
    pub launch_kind: StartupBindingKind,
    pub predecessor: Option<String>,
}

/// What a producer is told through its environment: who it is reporting for, and the attempt's budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupHandOff {
    pub binding: StartupBinding,
    pub role: StartupProducerRole,
    /// G of this attempt: the supervisor's own backstop for retries the controller would otherwise bound.
    pub budget_ms: u64,
    /// The filler slots this process inherited (`with_startup_pipe`); its startup stub closes them first.
    #[serde(default)]
    pub fillers: Vec<i32>,
}

pub fn encode_startup_hand_off(hand_off: &StartupHandOff) -> String {
    serde_json::to_string(hand_off).unwrap_or_default()
}

pub fn parse_startup_hand_off(value: Option<&str>) -> Option<StartupHandOff> {
    let raw = value.filter(|s| !s.is_empty())?;
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let obj = parsed.as_object()?;
    let role = match obj.get("role").and_then(Value::as_str)? {
        "supervisor" => StartupProducerRole::Supervisor,
        "host" => StartupProducerRole::Host,
        _ => return None,
    };
    let budget_ms = obj
        .get("budgetMs")
        .and_then(Value::as_f64)
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map(|ms| ms as u64)?;
    if budget_ms == 0 {
        return None;
    }
    let binding_val = obj.get("binding")?;
    let binding_obj = binding_val.as_object()?;
    let attempt = binding_obj.get("attempt")?.as_str()?.to_owned();
    let instance_id = binding_obj.get("instanceId")?.as_str()?.to_owned();
    let endpoint = binding_obj
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let generation = binding_obj
        .get("generation")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let launch_kind = match binding_obj.get("launchKind").and_then(Value::as_str) {
        Some("successor") => StartupBindingKind::Successor,
        _ => StartupBindingKind::Initial,
    };
    let predecessor = binding_obj
        .get("predecessor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let fillers = obj
        .get("fillers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|slot| slot.as_i64().and_then(|n| i32::try_from(n).ok()))
                .filter(|slot| *slot >= 3 && *slot < STARTUP_EVENT_FD)
                .collect()
        })
        .unwrap_or_default();
    Some(StartupHandOff {
        binding: StartupBinding {
            attempt,
            endpoint,
            instance_id,
            generation,
            launch_kind,
            predecessor,
        },
        role,
        budget_ms,
        fillers,
    })
}

pub fn startup_event_line(
    binding: &StartupBinding,
    role: StartupProducerRole,
    seq: u64,
    milestone: StartupMilestone,
) -> String {
    let line = json!({
        "v": STARTUP_EVENT_PROTOCOL_VERSION,
        "binding": binding,
        "role": role,
        "seq": seq,
        "milestone": milestone,
    });
    format!("{line}\n")
}

/// The launcher's wake-up to a successor supervisor on the same pipe, in the other direction: P7 resolved.
pub fn startup_grant_line(attempt: &str) -> String {
    format!(
        "{}\n",
        json!({"grant": "public_published", "attempt": attempt})
    )
}

/// The supervisor's frame around one host-child record line (without its LF).
pub fn relay_startup_line(line: &str) -> String {
    format!("{}\n", json!({"relay": "host", "line": line}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> StartupBinding {
        StartupBinding {
            attempt: "att-1".into(),
            endpoint: "/tmp/rpc.sock".into(),
            instance_id: "gen-1".into(),
            generation: 2,
            launch_kind: StartupBindingKind::Successor,
            predecessor: Some("gen-0".into()),
        }
    }

    #[test]
    fn hand_off_round_trips_and_filters_invalid_fillers() {
        let hand_off = StartupHandOff {
            binding: binding(),
            role: StartupProducerRole::Supervisor,
            budget_ms: 180_000,
            fillers: vec![5, 6],
        };
        let encoded = encode_startup_hand_off(&hand_off);
        assert_eq!(parse_startup_hand_off(Some(&encoded)), Some(hand_off));

        let with_bad_fillers = json!({
            "binding": binding(),
            "role": "host",
            "budgetMs": 1000,
            "fillers": [0, 2, 3, 15, 16, "x"]
        })
        .to_string();
        let parsed = parse_startup_hand_off(Some(&with_bad_fillers)).expect("parsed");
        assert_eq!(parsed.fillers, vec![3, 15]);
        assert_eq!(parse_startup_hand_off(None), None);
        assert_eq!(parse_startup_hand_off(Some("")), None);
        assert_eq!(parse_startup_hand_off(Some("{")), None);
    }

    #[test]
    fn wire_lines_end_with_lf_and_carry_expected_shapes() {
        let event = startup_event_line(
            &binding(),
            StartupProducerRole::Supervisor,
            1,
            StartupMilestone::SupervisorBootstrap,
        );
        assert!(event.ends_with('\n'));
        let parsed: Value = serde_json::from_str(event.trim_end()).expect("json");
        assert_eq!(parsed["v"], STARTUP_EVENT_PROTOCOL_VERSION);
        assert_eq!(parsed["milestone"], "supervisor_bootstrap");
        assert_eq!(parsed["seq"], 1);

        let grant = startup_grant_line("att-1");
        assert_eq!(
            serde_json::from_str::<Value>(grant.trim_end()).expect("grant"),
            json!({"grant": "public_published", "attempt": "att-1"})
        );
        let relay = relay_startup_line("{\"hello\":1}");
        assert_eq!(
            serde_json::from_str::<Value>(relay.trim_end()).expect("relay"),
            json!({"relay": "host", "line": "{\"hello\":1}"})
        );
    }
}
