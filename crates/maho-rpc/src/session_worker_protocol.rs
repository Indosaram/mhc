pub use crate::session_path_reservations::SessionWriteGrant;
use serde::{Deserialize, Serialize};
pub struct SessionWorkerLimits {
    pub workers: usize,
    pub requests: usize,
    pub control_requests: usize,
    pub control_bytes: usize,
    pub request_bytes: usize,
    pub output_bytes: usize,
    pub reservations: usize,
    pub open_ms: u64,
    pub control_ms: u64,
}
pub const SESSION_WORKER_LIMITS: SessionWorkerLimits = SessionWorkerLimits {
    workers: 20,
    requests: 64,
    control_requests: 4,
    control_bytes: 1024 * 1024,
    request_bytes: 16 * 1024 * 1024,
    output_bytes: 16 * 1024 * 1024,
    reservations: 64,
    open_ms: 30000,
    control_ms: 5000,
};
/// Whether an open step failed only because its worker died: the worker's request channel rejects what is pending
/// with a plain `session_closing` / `session_worker_exited`, and the claims and grants it held report a typed
/// `session_closing` (senpi#4037).
pub fn closed_by_worker_death(cause: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(typed) = cause.downcast_ref::<crate::session_registry::RpcSessionRegistryError>() {
        return typed.code == crate::rpc_types::RPC_ERROR_SESSION_CLOSING;
    }
    matches!(
        cause.to_string().as_str(),
        "session_closing" | "session_worker_exited"
    )
}

pub const fn worker_credit_code(grant: SessionWriteGrant) -> i32 {
    match grant {
        SessionWriteGrant::Granted => 1,
        SessionWriteGrant::Conflict => 2,
        SessionWriteGrant::Limit => 3,
    }
}
/// Wire values the host writes into a worker's wait signal; `conflict` is the generic denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerCreditCodes {
    pub granted: i32,
    pub conflict: i32,
    pub limit: i32,
}
pub const WORKER_CREDIT_CODES: WorkerCreditCodes = WorkerCreditCodes {
    granted: worker_credit_code(SessionWriteGrant::Granted),
    conflict: worker_credit_code(SessionWriteGrant::Conflict),
    limit: worker_credit_code(SessionWriteGrant::Limit),
};
/// Decodes a JSON number into `f64` even inside an internally tagged enum: there serde buffers the
/// payload, and with serde_json's `arbitrary_precision` (unified into the binary by other crates) a
/// buffered non-integer number arrives as a map that a plain `f64` field rejects.
fn json_f64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let number = serde_json::Number::deserialize(deserializer)?;
    number
        .as_f64()
        .ok_or_else(|| serde::de::Error::custom("number is not representable as f64"))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerDisplay {
    #[serde(deserialize_with = "json_f64")]
    pub revision: f64,
    #[serde(deserialize_with = "json_f64")]
    pub width: f64,
    pub rendered: bool,
    pub capabilities: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSnapshot {
    pub state: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
    pub live_session_paths: Vec<String>,
    pub busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_busy: Option<bool>,
    pub streaming: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HostToSessionWorker {
    Prepare {
        request: u64,
        configuration: serde_json::Value,
        profile: serde_json::Value,
    },
    Commit {
        request: u64,
    },
    Bind {
        request: u64,
        session_id: String,
        display: WorkerDisplay,
        #[serde(skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        roles: Option<Vec<String>>,
    },
    Command {
        request: u64,
        command: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        roles: Option<Vec<String>>,
        display: WorkerDisplay,
    },
    Display {
        display: WorkerDisplay,
    },
    CancelUi,
    CancelGrant { id: String },
    Close,
    /// The host's answer to one blocking worker exchange (native stand-in for senpi's
    /// `SharedArrayBuffer` wait signal: the worker is a process, so the grant crosses the pipe).
    Credit {
        signal: u64,
        code: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<WorkerDisplay>,
    },
}
/// The grant a credit code encodes (inverse of `worker_credit_code`); anything else is a timeout.
pub const fn grant_from_credit_code(code: i32) -> Option<SessionWriteGrant> {
    match code {
        1 => Some(SessionWriteGrant::Granted),
        2 => Some(SessionWriteGrant::Conflict),
        3 => Some(SessionWriteGrant::Limit),
        _ => None,
    }
}
/// `SessionWorkerToHost` on the process wire: every wait signal is named by an id the host echoes
/// back in `HostToSessionWorker::Credit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionWorkerWire {
    Prepared {
        request: u64,
        session_path: String,
    },
    Ready {
        request: u64,
        snapshot: WorkerSnapshot,
    },
    Result {
        request: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Reserve {
        path: String,
        signal: u64,
    },
    Snapshot {
        snapshot: WorkerSnapshot,
        signal: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settled: Option<bool>,
    },
    ControlDone {
        control: WorkerControlKind,
    },
    Output {
        record: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        signal: u64,
        activity: WorkerActivity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<WorkerSnapshot>,
    },
    Width {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        #[serde(deserialize_with = "json_f64")]
        width: f64,
        signal: u64,
    },
    Capabilities {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        capabilities: Vec<String>,
        signal: u64,
    },
    RequestClose,
    Failure {
        error: String,
    },
}
#[derive(Clone)]
pub enum SessionWorkerToHost {
    Prepared {
        request: u64,
        session_path: String,
    },
    Ready {
        request: u64,
        snapshot: WorkerSnapshot,
    },
    Result {
        request: u64,
        error: Option<String>,
    },
    Reserve {
        path: String,
        signal: crate::session_worker_signals::WorkerSignal,
    },
    Snapshot {
        snapshot: WorkerSnapshot,
        signal: crate::session_worker_signals::WorkerSignal,
        settled: Option<bool>,
    },
    ControlDone {
        control: WorkerControlKind,
    },
    Output {
        record: serde_json::Value,
        connection: Option<String>,
        signal: crate::session_worker_signals::WorkerSignal,
        activity: WorkerActivity,
        snapshot: Option<WorkerSnapshot>,
    },
    Width {
        connection: Option<String>,
        width: f64,
        signal: crate::session_worker_signals::WorkerSignal,
    },
    Capabilities {
        connection: Option<String>,
        capabilities: Vec<String>,
        signal: crate::session_worker_signals::WorkerSignal,
    },
    RequestClose,
    Failure {
        error: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerControlKind {
    Display,
    CancelUi,
}
/// The launch profile on the worker wire (senpi posts the `RpcSessionLaunchProfile` object itself).
pub fn profile_to_wire(
    profile: &crate::session_registry::RpcSessionLaunchProfile,
) -> serde_json::Value {
    let mut value = serde_json::json!({"cwd":profile.runtime.cwd});
    if let Some(preset) = &profile.runtime.permission_preset {
        value["permissionPreset"] = preset.clone().into();
    }
    if let Some((provider, model_id)) = &profile.runtime.creation_model {
        value["creationModel"] = serde_json::json!({"provider":provider,"modelId":model_id});
    }
    if let Some(level) = &profile.runtime.initial_thinking_level {
        value["initialThinkingLevel"] = level.clone().into();
    }
    if let Some(auto_title) = profile.runtime.auto_title {
        value["autoTitle"] = auto_title.into();
    }
    if let Some(path) = &profile.session_path {
        value["sessionPath"] = path.clone().into();
    }
    if let Some(id) = &profile.durable_session_id {
        value["durableSessionId"] = id.clone().into();
    }
    if let Some(kind) = profile.session_kind {
        value["sessionKind"] = if kind == maho_ext_api::SessionKind::Worker {
            "worker"
        } else {
            "interactive"
        }
        .into();
    }
    if let Some(context) = &profile.session_context {
        value["sessionContext"] = serde_json::json!(context);
    }
    if profile.attended == Some(false) {
        value["attended"] = false.into();
    }
    if let Some(surface) = &profile.prompt_surface {
        value["promptSurface"] = surface.clone().into();
    }
    if let Some(engine) = &profile.browser_engine {
        value["browserEngine"] = engine.clone().into();
    }
    if let Some(fallback) = &profile.retry_fallback {
        value["retryFallback"] = fallback.clone();
    }
    if let Some(endpoint) = &profile.holder_endpoint {
        let mut holder = serde_json::Map::new();
        if let Some(instance) = &endpoint.host_instance {
            holder.insert("host_instance".into(), instance.clone().into());
        }
        if let Some(socket) = &endpoint.host_socket {
            holder.insert("host_socket".into(), socket.clone().into());
        }
        if let Some(dir) = &endpoint.host_daemon_dir {
            holder.insert("host_daemon_dir".into(), dir.clone().into());
        }
        value["holderEndpoint"] = serde_json::Value::Object(holder);
    }
    value
}
pub fn profile_from_wire(
    value: &serde_json::Value,
) -> Result<crate::session_registry::RpcSessionLaunchProfile, String> {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let cwd = text("cwd").ok_or("Invalid worker launch profile: cwd")?;
    let creation_model = value.get("creationModel").and_then(|model| {
        Some((
            model.get("provider")?.as_str()?.to_owned(),
            model.get("modelId")?.as_str()?.to_owned(),
        ))
    });
    let session_context = match value.get("sessionContext") {
        Some(context) => {
            Some(serde_json::from_value(context.clone()).map_err(|error| error.to_string())?)
        }
        None => None,
    };
    Ok(crate::session_registry::RpcSessionLaunchProfile {
        runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
            cwd,
            permission_preset: text("permissionPreset"),
            creation_model,
            initial_thinking_level: text("initialThinkingLevel"),
            auto_title: value.get("autoTitle").and_then(serde_json::Value::as_bool),
                prompt_size_inventory: None,
        },
        session_path: text("sessionPath"),
        durable_session_id: text("durableSessionId"),
        session_kind: text("sessionKind").map(|kind| {
            if kind == "worker" {
                maho_ext_api::SessionKind::Worker
            } else {
                maho_ext_api::SessionKind::Interactive
            }
        }),
        session_context,
        attended: value
            .get("attended")
            .and_then(serde_json::Value::as_bool)
            .filter(|attended| !attended),
        prompt_surface: text("promptSurface"),
        browser_engine: text("browserEngine"),
        retry_fallback: value.get("retryFallback").cloned(),
        holder_endpoint: value.get("holderEndpoint").and_then(|holder| {
            let field = |key: &str| {
                holder
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            Some(maho_core::session_holders::SessionHolderEndpoint {
                host_instance: field("host_instance"),
                host_socket: field("host_socket"),
                host_daemon_dir: field("host_daemon_dir"),
            })
            .filter(|endpoint| {
                endpoint != &maho_core::session_holders::SessionHolderEndpoint::default()
            })
        }),
    })
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerActivity {
    pub busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_busy: Option<bool>,
    pub streaming: bool,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_death_is_a_closing_channel_not_a_generic_failure() {
        let closing = crate::session_registry::RpcSessionRegistryError::new(
            crate::rpc_types::RPC_ERROR_SESSION_CLOSING,
            None,
            None,
        );
        assert!(closed_by_worker_death(&closing));
        // A typed error with any other code is a real refusal, not a death.
        let refused = crate::session_registry::RpcSessionRegistryError::new(
            "session_path_in_use",
            None,
            None,
        );
        assert!(!closed_by_worker_death(&refused));
        // The request channel rejects with plain messages.
        for message in ["session_closing", "session_worker_exited"] {
            let plain = std::io::Error::other(message);
            assert!(closed_by_worker_death(&plain));
        }
        assert!(!closed_by_worker_death(&std::io::Error::other(
            "session_worker_protocol_error"
        )));
    }
    #[test]
    fn credit_codes_struct_matches_the_grant_encoding() {
        assert_eq!(
            WORKER_CREDIT_CODES,
            WorkerCreditCodes {
                granted: 1,
                conflict: 2,
                limit: 3
            }
        );
        assert_eq!(
            worker_credit_code(SessionWriteGrant::Granted),
            WORKER_CREDIT_CODES.granted
        );
        assert_eq!(
            worker_credit_code(SessionWriteGrant::Conflict),
            WORKER_CREDIT_CODES.conflict
        );
        assert_eq!(
            worker_credit_code(SessionWriteGrant::Limit),
            WORKER_CREDIT_CODES.limit
        );
    }
    #[test]
    fn optional_snapshot_fields_are_omitted() {
        let snapshot = WorkerSnapshot {
            state: serde_json::json!({}),
            session_path: None,
            live_session_paths: vec![],
            busy: false,
            handoff_busy: None,
            streaming: false,
        };
        let wire = serde_json::to_value(snapshot).unwrap();
        assert!(wire.get("sessionPath").is_none());
        assert!(wire.get("handoffBusy").is_none());
        assert_eq!(wire["liveSessionPaths"], serde_json::json!([]));
    }
    #[test]
    fn credit_codes_round_trip_to_grants() {
        for grant in [
            SessionWriteGrant::Granted,
            SessionWriteGrant::Conflict,
            SessionWriteGrant::Limit,
        ] {
            assert_eq!(
                grant_from_credit_code(worker_credit_code(grant)),
                Some(grant)
            );
        }
        assert_eq!(grant_from_credit_code(0), None);
    }
    #[test]
    fn process_wire_envelopes_keep_tags_and_signal_ids() {
        for value in [
            serde_json::json!({"type":"reserve","path":"/s","signal":7}),
            serde_json::json!({"type":"output","record":{"type":"agent_end"},"connection":"peer","signal":8,"activity":{"busy":true,"streaming":false}}),
            serde_json::json!({"type":"result","request":3}),
            serde_json::json!({"type":"request_close"}),
        ] {
            let parsed: SessionWorkerWire = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        }
        let credit = serde_json::json!({"type":"credit","signal":7,"code":1});
        let parsed: HostToSessionWorker = serde_json::from_value(credit.clone()).unwrap();
        assert_eq!(
            parsed,
            HostToSessionWorker::Credit {
                signal: 7,
                code: 1,
                display: None
            }
        );
        assert_eq!(serde_json::to_value(parsed).unwrap(), credit);
    }
    #[test]
    fn launch_profile_round_trips_over_the_worker_wire() {
        let profile = crate::session_registry::RpcSessionLaunchProfile {
            runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                cwd: "/w".into(),
                permission_preset: Some("p".into()),
                creation_model: Some(("faux".into(), "m".into())),
                initial_thinking_level: Some("high".into()),
                auto_title: Some(false),
                prompt_size_inventory: None,
            },
            session_path: Some("/w/s.jsonl".into()),
            durable_session_id: Some("d".into()),
            session_kind: Some(maho_ext_api::SessionKind::Worker),
            session_context: Some(std::collections::BTreeMap::from([("k".into(), "v".into())])),
            attended: Some(false),
            prompt_surface: Some("app".into()),
            browser_engine: Some("builtin".into()),
            retry_fallback: Some(
                serde_json::json!({"modelFallback":true,"fallbackChains":{"a":["b"]}}),
            ),
            holder_endpoint: Some(maho_core::session_holders::SessionHolderEndpoint {
                host_instance: Some("i".into()),
                host_socket: None,
                host_daemon_dir: Some("/d".into()),
            }),
        };
        assert_eq!(
            profile_from_wire(&profile_to_wire(&profile)).unwrap(),
            profile
        );
        let bare = crate::session_registry::RpcSessionLaunchProfile {
            runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                cwd: "/w".into(),
                ..Default::default()
            },
            session_path: None,
            durable_session_id: None,
            session_kind: None,
            ..Default::default()
        };
        assert_eq!(profile_from_wire(&profile_to_wire(&bare)).unwrap(), bare);
        assert!(profile_from_wire(&serde_json::json!({})).is_err());
    }
}
