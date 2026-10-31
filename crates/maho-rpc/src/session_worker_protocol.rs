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
/// The isolate's transcript journal up to bootstrap (senpi `SessionBootstrap` on the worker wire). Upstream's
/// `epochSignal` SharedArrayBuffer has no Rust counterpart: the replacement nonce is the host's, and the host
/// sets it when it bootstraps the lane from this frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerBootstrap {
    pub transcript: Vec<maho_core::session_transcript_journal::TranscriptTransition>,
}
/// `RecoveryCarrier` on the worker wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerRecoveryCarrier {
    pub message_id: String,
    pub epoch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_entry_id: Option<String>,
}
impl From<WorkerRecoveryCarrier> for maho_core::session_recovery_identity::RecoveryCarrier {
    fn from(carrier: WorkerRecoveryCarrier) -> Self {
        Self {
            message_id: carrier.message_id,
            epoch: carrier.epoch,
            durable_entry_id: carrier.durable_entry_id,
        }
    }
}
impl From<maho_core::session_recovery_identity::RecoveryCarrier> for WorkerRecoveryCarrier {
    fn from(carrier: maho_core::session_recovery_identity::RecoveryCarrier) -> Self {
        Self {
            message_id: carrier.message_id,
            epoch: carrier.epoch,
            durable_entry_id: carrier.durable_entry_id,
        }
    }
}
/// senpi `withinFrameLimit`: a transition whose entry cannot fit one worker frame is marked unavailable.
pub fn within_frame_limit(
    transitions: Vec<maho_core::session_transcript_journal::TranscriptTransition>,
) -> Vec<maho_core::session_transcript_journal::TranscriptTransition> {
    use maho_core::session_transcript_journal::{
        TranscriptTransition, TranscriptUnavailableReason,
    };
    // Reserve fixed metadata space; content is charged in the same raw UTF-8 bytes as the accepted tail.
    const ENTRY_OVERHEAD: usize = 1024;
    transitions
        .into_iter()
        .map(|transition| {
            let content = match &transition {
                TranscriptTransition::Append { entry, .. } => entry.as_ref().map_or(0, String::len),
                _ => 0,
            };
            if content + ENTRY_OVERHEAD > SESSION_WORKER_LIMITS.output_bytes {
                TranscriptTransition::Unavailable {
                    reason: TranscriptUnavailableReason::TransitionOverWorkerFrameLimit,
                }
            } else {
                transition
            }
        })
        .collect()
}
/// A worker's refusal crosses the process boundary as text. An `open_failed: <reason>` refusal is rebuilt as the
/// typed registry error, so a worker host answers `open_session` exactly as the in-process registry does
/// (senpi#2898); any other text is returned as it stands.
pub fn typed_worker_refusal(
    cause: String,
) -> Result<crate::session_registry::RpcSessionRegistryError, String> {
    match cause.strip_prefix("open_failed: ") {
        Some(reason) => Ok(crate::session_registry::RpcSessionRegistryError::new(
            "open_failed",
            Some(reason),
            None,
        )),
        None => Err(cause),
    }
}
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
        /// The host store's mirrored records as of the prepare (senpi#3695 `mirrorSnapshot()`).
        #[serde(default)]
        subscription_limits: Vec<serde_json::Value>,
    },
    /// `acceptance`: the host owns an acceptance lane, so the isolate sends a bootstrap and ordered output frames.
    Commit {
        request: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acceptance: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery_epoch: Option<String>,
    },
    Bind {
        request: u64,
        session_id: String,
        display: WorkerDisplay,
        #[serde(skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        roles: Option<Vec<String>>,
        /// The host enforces private MCP forms: the worker binds its forms through the host (senpi `mcpFormsEnforced`).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mcp_forms_enforced: bool,
    },
    /// The host's answer to `mcp_form_bind`: the binding epoch and its current grant version.
    #[serde(rename = "mcp_form_bound")]
    McpFormBound {
        id: String,
        epoch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    /// The binding's grant changed (or a `mcp_form_ready` was answered, echoing its `id`).
    #[serde(rename = "mcp_form_availability")]
    McpFormAvailability {
        epoch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// One created form settled on the host; `outcome` is the `McpFormOutcome` wire union.
    #[serde(rename = "mcp_form_settlement")]
    McpFormSettlement {
        id: String,
        epoch: String,
        settlement_id: String,
        outcome: serde_json::Value,
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
    /// An attach that names another surface moves the live session to it (senpi `prompt_surface`).
    PromptSurface {
        request: u64,
        surface: String,
    },
    /// An attach that names another browser engine moves the live session to it (senpi `browser_engine`).
    BrowserEngine {
        request: u64,
        engine: String,
    },
    /// Sent even when the record already names it: only the worker sees its live session (senpi#2842).
    PermissionPreset {
        request: u64,
        preset: String,
    },
    Display {
        display: WorkerDisplay,
    },
    CancelUi,
    CancelGrant {
        id: String,
    },
    /// The host's subscription limit records, mirrored into the worker's store (senpi#3695).
    SubscriptionLimits {
        accounts: Vec<serde_json::Value>,
    },
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
    /// Private MCP forms (senpi#4254): these never pass through output, snapshots or replay.
    #[serde(rename = "mcp_form_bind")]
    McpFormBind {
        id: String,
        durable_id: String,
    },
    #[serde(rename = "mcp_form_ready")]
    McpFormReady {
        id: String,
        epoch: String,
    },
    #[serde(rename = "mcp_form_release")]
    McpFormRelease {
        epoch: String,
    },
    #[serde(rename = "mcp_form_cancel")]
    McpFormCancel {
        id: String,
        epoch: String,
        lifetime: McpFormCancelLifetime,
    },
    /// NATIVE-DIVERGENCE(senpi SharedArrayBuffer operationFence): the worker is a process, so the operation fence
    /// is the host-side `operation` abort a `mcp_form_cancel { lifetime: "operation" }` sets; the frames are
    /// ordered on one pipe, so the cancel lands before any later host dispatch.
    #[serde(rename = "mcp_form_create")]
    McpFormCreate {
        id: String,
        epoch: String,
        version: String,
        input: crate::mcp_form_types::McpFormInputWire,
    },
    Prepared {
        request: u64,
        session_path: String,
    },
    Ready {
        request: u64,
        snapshot: WorkerSnapshot,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bootstrap: Option<WorkerBootstrap>,
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
        /// One per output frame of this worker incarnation, from 0; continuity only, never the accepted sequence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ordinal: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<WorkerRecoveryCarrier>,
        /// Transcript transitions the isolate's manager committed before this record, accepted just before it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transitions: Option<Vec<maho_core::session_transcript_journal::TranscriptTransition>>,
        /// The next incarnation's baseline, on the `session_replaced` record that starts it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bootstrap: Option<WorkerBootstrap>,
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
    /// A record a turn in the worker reported, mirrored into the host's store (senpi#3695).
    SubscriptionLimits {
        limits: serde_json::Value,
    },
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
    /// A record a turn in the worker reported, mirrored into the host's store (senpi#3695).
    SubscriptionLimits {
        limits: serde_json::Value,
    },
    Failure {
        error: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFormCancelLifetime {
    Operation,
    Presentation,
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
    if let Some(spawned_by) = &profile.spawned_by
        && let Ok(ancestry) = serde_json::to_value(spawned_by)
    {
        value["spawnedBy"] = ancestry;
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
        spawned_by: value
            .get("spawnedBy")
            .and_then(|ancestry| serde_json::from_value(ancestry.clone()).ok()),
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
    fn subscription_limit_mirror_messages_cross_both_ways() {
        let record = serde_json::json!({"provider":"p","account":"a","windows":[],"reportedAt":"t","revision":1});
        let host: HostToSessionWorker = serde_json::from_value(
            serde_json::json!({"type":"subscription_limits","accounts":[record.clone()]}),
        )
        .unwrap();
        assert_eq!(
            host,
            HostToSessionWorker::SubscriptionLimits {
                accounts: vec![record.clone()]
            }
        );
        let worker = serde_json::to_value(SessionWorkerWire::SubscriptionLimits {
            limits: record.clone(),
        })
        .unwrap();
        assert_eq!(
            worker,
            serde_json::json!({"type":"subscription_limits","limits":record})
        );
        // An older host's prepare carries no records.
        let prepare: HostToSessionWorker = serde_json::from_value(
            serde_json::json!({"type":"prepare","request":1,"configuration":{},"profile":{}}),
        )
        .unwrap();
        assert!(matches!(
            prepare,
            HostToSessionWorker::Prepare { subscription_limits, .. } if subscription_limits.is_empty()
        ));
    }
    #[test]
    fn private_form_messages_cross_both_ways_in_their_upstream_shape() {
        let bound: HostToSessionWorker = serde_json::from_value(
            serde_json::json!({"type":"mcp_form_bound","id":"i","epoch":"e"}),
        )
        .unwrap();
        assert_eq!(
            bound,
            HostToSessionWorker::McpFormBound {
                id: "i".into(),
                epoch: "e".into(),
                version: None
            }
        );
        let settlement = serde_json::to_value(HostToSessionWorker::McpFormSettlement {
            id: "i".into(),
            epoch: "e".into(),
            settlement_id: "s".into(),
            outcome: serde_json::json!({"kind":"processing_failure"}),
        })
        .unwrap();
        assert_eq!(settlement["settlementId"], "s");
        let cancel = serde_json::to_value(SessionWorkerWire::McpFormCancel {
            id: "i".into(),
            epoch: "e".into(),
            lifetime: McpFormCancelLifetime::Operation,
        })
        .unwrap();
        assert_eq!(
            cancel,
            serde_json::json!({"type":"mcp_form_cancel","id":"i","epoch":"e","lifetime":"operation"})
        );
        let bind: SessionWorkerWire = serde_json::from_value(
            serde_json::json!({"type":"mcp_form_bind","id":"i","durableId":"d"}),
        )
        .unwrap();
        assert_eq!(
            bind,
            SessionWorkerWire::McpFormBind {
                id: "i".into(),
                durable_id: "d".into()
            }
        );
        let enforced = serde_json::to_value(HostToSessionWorker::Bind {
            request: 1,
            session_id: "s".into(),
            display: WorkerDisplay {
                revision: 1.,
                width: 80.,
                rendered: false,
                capabilities: vec![],
            },
            connection: None,
            roles: None,
            mcp_forms_enforced: true,
        })
        .unwrap();
        assert_eq!(enforced["mcpFormsEnforced"], true);
    }
    #[test]
    fn attach_setters_cross_in_their_upstream_shape() {
        for (wire, message) in [
            (
                serde_json::json!({"type":"prompt_surface","request":3,"surface":"app"}),
                HostToSessionWorker::PromptSurface {
                    request: 3,
                    surface: "app".into(),
                },
            ),
            (
                serde_json::json!({"type":"browser_engine","request":4,"engine":"builtin"}),
                HostToSessionWorker::BrowserEngine {
                    request: 4,
                    engine: "builtin".into(),
                },
            ),
            (
                serde_json::json!({"type":"permission_preset","request":5,"preset":"guarded"}),
                HostToSessionWorker::PermissionPreset {
                    request: 5,
                    preset: "guarded".into(),
                },
            ),
        ] {
            assert_eq!(serde_json::to_value(&message).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<HostToSessionWorker>(wire).unwrap(),
                message
            );
        }
    }
    #[test]
    fn an_open_failed_refusal_is_rebuilt_as_the_typed_registry_error() {
        let typed = typed_worker_refusal("open_failed: no model".into()).unwrap();
        assert_eq!(typed.code, "open_failed");
        assert_eq!(typed.to_string(), "open_failed: no model");
        assert_eq!(
            typed_worker_refusal("session_worker_exited".into()).unwrap_err(),
            "session_worker_exited"
        );
    }
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
            spawned_by: Some(maho_core::session_lifecycle_types::SpawnedBy {
                parent_session_id: "p".into(),
                root_session_id: "r".into(),
                task_id: "t".into(),
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
    #[test]
    fn oversized_transitions_become_unavailable_and_ordered_frames_round_trip() {
        use maho_core::session_transcript_journal::{
            TranscriptTransition, TranscriptUnavailableReason,
        };
        let big = TranscriptTransition::Append {
            id: "e".into(),
            entry: Some("x".repeat(SESSION_WORKER_LIMITS.output_bytes)),
            persistence: None,
            owning_message_id: None,
            recovery_epoch: None,
        };
        let leaf = TranscriptTransition::Leaf { leaf_id: None };
        assert_eq!(
            within_frame_limit(vec![big, leaf.clone()]),
            [
                TranscriptTransition::Unavailable {
                    reason: TranscriptUnavailableReason::TransitionOverWorkerFrameLimit
                },
                leaf
            ]
        );
        let commit =
            serde_json::json!({"type":"commit","request":2,"acceptance":true,"recoveryEpoch":"n"});
        let parsed: HostToSessionWorker = serde_json::from_value(commit.clone()).expect("commit");
        assert_eq!(serde_json::to_value(parsed).expect("json"), commit);
        let output = serde_json::json!({"type":"output","record":{"type":"agent_start"},"signal":1,
            "activity":{"busy":false,"streaming":false},"ordinal":0,
            "recovery":{"messageId":"m","epoch":"n"},"transitions":[{"kind":"leaf","leafId":null}]});
        let parsed: SessionWorkerWire = serde_json::from_value(output).expect("output");
        let SessionWorkerWire::Output {
            ordinal,
            recovery,
            transitions,
            ..
        } = parsed
        else {
            panic!("output frame");
        };
        assert_eq!(ordinal, Some(0));
        assert_eq!(
            recovery.map(|carrier| carrier.message_id).as_deref(),
            Some("m")
        );
        assert_eq!(transitions.map(|t| t.len()), Some(1));
    }
}
