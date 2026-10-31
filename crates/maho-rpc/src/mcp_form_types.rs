//! Private MCP form types shared by the host authority, the worker bridge and the MCP client (senpi
//! `core/mcp-form-types.ts`).
//!
//! Upstream this file lives in core (census senpi-core-rest-54/55) and `bindPrivateMcpForms` hangs the binding on
//! the session's extension UI. maho-core has no form binding yet, so maho-rpc keeps this mirror and a registry
//! keyed by durable session id (the key `mcp_grant_presentations` already uses). Once maho-core owns it, this
//! module re-exports that.
use maho_ai::{types::BoxFuture, utils::abort::AbortSignal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};

/// `McpFormScope["presenterKind"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPresenterKind {
    Desktop,
    Gateway,
    LocalTui,
}
impl McpPresenterKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "desktop" => Some(Self::Desktop),
            "gateway" => Some(Self::Gateway),
            "local_tui" => Some(Self::LocalTui),
            _ => None,
        }
    }
}

/// The trusted presenter grant (senpi `McpFormScope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFormScope {
    pub host_instance: String,
    pub durable_session_id: String,
    pub binding_epoch: String,
    pub attachment_id: String,
    pub attachment_epoch: String,
    pub connection_id: String,
    pub connection_epoch: String,
    pub presenter_principal_id: String,
    pub presenter_generation: String,
    pub presentation_binding_id: String,
    pub presenter_kind: McpPresenterKind,
    /// Always `mcp_form`.
    pub purpose: String,
}

/// Who settled an outcome. Only `PresenterAction` carries human admission (senpi's provenance union).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpFormSource {
    PresenterAction,
    HostTermination,
}

/// senpi `McpFormOutcome`: `result` carries an `MrtrInputResponse` (`{action:"accept",content}` or
/// `{action:"decline"|"cancel"}`).
#[derive(Debug, Clone, PartialEq)]
pub enum McpFormOutcomeKind {
    Result(Value),
    ProcessingFailure,
    OperationAborted,
}
#[derive(Debug, Clone, PartialEq)]
pub struct McpFormOutcome {
    pub kind: McpFormOutcomeKind,
    pub source: McpFormSource,
}
impl McpFormOutcome {
    pub fn terminated(kind: McpFormOutcomeKind) -> Self {
        Self {
            kind,
            source: McpFormSource::HostTermination,
        }
    }
    pub fn processing_failure() -> Self {
        Self::terminated(McpFormOutcomeKind::ProcessingFailure)
    }
    pub fn operation_aborted() -> Self {
        Self::terminated(McpFormOutcomeKind::OperationAborted)
    }
    pub fn human_admission(&self) -> bool {
        self.source == McpFormSource::PresenterAction
    }
    pub fn to_wire(&self) -> Value {
        let mut value = match &self.kind {
            McpFormOutcomeKind::Result(result) => {
                serde_json::json!({"kind":"result","result":result})
            }
            McpFormOutcomeKind::ProcessingFailure => {
                serde_json::json!({"kind":"processing_failure"})
            }
            McpFormOutcomeKind::OperationAborted => serde_json::json!({"kind":"operation_aborted"}),
        };
        value["source"] = match self.source {
            McpFormSource::PresenterAction => "presenter_action",
            McpFormSource::HostTermination => "host_termination",
        }
        .into();
        value["humanAdmission"] = self.human_admission().into();
        value
    }
    /// The union exactly: presenter provenance requires `humanAdmission: true` and the reverse.
    pub fn from_wire(value: &Value) -> Option<Self> {
        let source = match (
            value.get("source").and_then(Value::as_str),
            value.get("humanAdmission").and_then(Value::as_bool),
        ) {
            (Some("presenter_action"), Some(true)) => McpFormSource::PresenterAction,
            (Some("host_termination"), Some(false)) => McpFormSource::HostTermination,
            _ => return None,
        };
        let kind = match value.get("kind").and_then(Value::as_str)? {
            "result" => McpFormOutcomeKind::Result(value.get("result")?.clone()),
            "processing_failure" => McpFormOutcomeKind::ProcessingFailure,
            "operation_aborted" => McpFormOutcomeKind::OperationAborted,
            _ => return None,
        };
        Some(Self { kind, source })
    }
}

/// The serializable part of `McpFormInput` (what the worker posts in `mcp_form_create`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFormInputWire {
    pub operation_id: String,
    pub input_id: String,
    pub round: f64,
    /// `{ label, source: "user" | "project" | "plugin" }`.
    pub server: Value,
    pub message: String,
    pub schema: Value,
    /// Remaining UI budget, measured by the host's monotonic clock.
    pub deadline_ms: f64,
}

/// senpi `McpFormInput`.
#[derive(Clone)]
pub struct McpFormInput {
    pub wire: McpFormInputWire,
    pub operation_signal: AbortSignal,
    /// Host reads a worker-owned abort fence at the terminal latch, without an IPC round trip.
    pub operation_current: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub presentation_signal: AbortSignal,
}
impl McpFormInput {
    pub fn operation_stale(&self) -> bool {
        self.operation_signal.aborted()
            || self
                .operation_current
                .as_ref()
                .is_some_and(|current| !current())
    }
}

pub type PresentForm =
    Arc<dyn Fn(McpFormInput) -> BoxFuture<'static, McpFormOutcome> + Send + Sync>;

/// senpi `McpFormPresentation`.
#[derive(Clone)]
pub struct McpFormPresentation {
    pub signal: AbortSignal,
    pub ready: Arc<dyn Fn() -> bool + Send + Sync>,
    pub present: PresentForm,
    /// Worker-backed presenters recheck host readiness before each network attempt.
    pub synchronize: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
}

pub type Listener = Arc<dyn Fn() + Send + Sync>;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;

/// senpi `McpFormBinding`. Identity is `id`: the TS object identity the authority compares.
pub struct McpFormBinding {
    pub id: u64,
    pub epoch: String,
    pub signal: AbortSignal,
    pub capture: Box<dyn Fn() -> Option<McpFormPresentation> + Send + Sync>,
    pub dispose: Box<dyn Fn() + Send + Sync>,
    pub version: Option<Box<dyn Fn() -> Option<String> + Send + Sync>>,
    pub subscribe: Option<Box<dyn Fn(Listener) -> Unsubscribe + Send + Sync>>,
}
static NEXT_BINDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
impl McpFormBinding {
    pub fn next_id() -> u64 {
        NEXT_BINDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    pub fn capture(&self) -> Option<McpFormPresentation> {
        (self.capture)()
    }
    pub fn dispose(&self) {
        (self.dispose)()
    }
    pub fn version(&self) -> Option<String> {
        self.version.as_ref().and_then(|version| version())
    }
}

type Registry = std::collections::BTreeMap<String, Vec<(u64, Arc<McpFormBinding>)>>;
static PRIVATE_FORMS: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> std::sync::MutexGuard<'static, Registry> {
    PRIVATE_FORMS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// Removes one registration when dropped.
pub struct PrivateFormsRegistration {
    durable_id: String,
    id: u64,
}
impl Drop for PrivateFormsRegistration {
    fn drop(&mut self) {
        let mut registry = registry();
        if let Some(entries) = registry.get_mut(&self.durable_id) {
            entries.retain(|(id, _)| *id != self.id);
            if entries.is_empty() {
                registry.remove(&self.durable_id);
            }
        }
    }
}
/// senpi `bindPrivateMcpForms`: an enforced binding is part of the host-issued presenter identity, not a UI
/// capability declaration. The newest registration for a session wins.
pub fn bind_private_mcp_forms(
    durable_id: &str,
    binding: Arc<McpFormBinding>,
) -> PrivateFormsRegistration {
    let id = binding.id;
    registry()
        .entry(durable_id.to_owned())
        .or_default()
        .push((id, binding));
    PrivateFormsRegistration {
        durable_id: durable_id.to_owned(),
        id,
    }
}
/// senpi `privateMcpForms`.
pub fn private_mcp_forms(durable_id: &str) -> Option<Arc<McpFormBinding>> {
    registry()
        .get(durable_id)
        .and_then(|entries| entries.last())
        .map(|(_, binding)| Arc::clone(binding))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outcome_wire_keeps_the_provenance_union_exact() {
        let human = McpFormOutcome {
            kind: McpFormOutcomeKind::Result(serde_json::json!({"action":"decline"})),
            source: McpFormSource::PresenterAction,
        };
        assert_eq!(McpFormOutcome::from_wire(&human.to_wire()), Some(human));
        let mut forged = McpFormOutcome::processing_failure().to_wire();
        forged["humanAdmission"] = true.into();
        assert_eq!(McpFormOutcome::from_wire(&forged), None);
    }
    #[test]
    fn private_forms_registry_drops_with_its_registration() {
        let binding = Arc::new(McpFormBinding {
            id: McpFormBinding::next_id(),
            epoch: "e".into(),
            signal: maho_ai::utils::abort::AbortController::new().signal(),
            capture: Box::new(|| None),
            dispose: Box::new(|| {}),
            version: None,
            subscribe: None,
        });
        let registration = bind_private_mcp_forms("durable-types-test", binding);
        assert!(private_mcp_forms("durable-types-test").is_some());
        drop(registration);
        assert!(private_mcp_forms("durable-types-test").is_none());
    }
}
