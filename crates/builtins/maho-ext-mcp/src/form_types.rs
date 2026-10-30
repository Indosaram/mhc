//! Port of senpi core/mcp-form-types.ts: the host-issued private MCP form presenter contract.
//!
//! senpi keeps this file in core so the RPC and interactive hosts can bind presenters; in mhc the MCP
//! builtin owns it (cross-lane: maho-rpc / maho-interactive bind through these symbols).
use crate::modern::{elicitation::MrtrInputResponse, form_schema::ValidatedMcpFormSchema};
use futures::future::BoxFuture;
use maho_ext_api::{AbortSignal, ExtensionUi};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub purpose: McpFormPurpose,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPresenterKind {
    Desktop,
    Gateway,
    LocalTui,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFormPurpose {
    McpForm,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFormEnvelope {
    pub session_id: String,
    pub binding_epoch: String,
    pub attachment_epoch: String,
    pub grant_id: String,
    pub dialog_id: String,
    pub dialog_token: String,
    pub form_revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpFormServerSource {
    User,
    Project,
    Plugin,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpFormServer {
    pub label: String,
    pub source: McpFormServerSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpFormRequestType {
    #[serde(rename = "mcp_form_request")]
    McpFormRequest,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFormRequest {
    #[serde(rename = "type")]
    pub kind: McpFormRequestType,
    #[serde(flatten)]
    pub envelope: McpFormEnvelope,
    pub server: McpFormServer,
    pub message: String,
    pub schema: ValidatedMcpFormSchema,
    pub deadline_at_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFormResolvedReason {
    Answered,
    Declined,
    Cancelled,
    ProcessingFailed,
    OperationAborted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpFormResolvedType {
    #[serde(rename = "mcp_form_resolved")]
    McpFormResolved,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpFormResolved {
    #[serde(rename = "type")]
    pub kind: McpFormResolvedType,
    pub session_id: String,
    pub grant_id: String,
    pub dialog_id: String,
    pub reason: McpFormResolvedReason,
}

#[derive(Clone, Debug, PartialEq)]
pub enum McpFormOutcomeKind {
    Result(MrtrInputResponse),
    ProcessingFailure,
    OperationAborted,
}
/// `presenter_action` outcomes carry `humanAdmission: true`; `host_termination` ones never do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpFormOutcomeSource {
    PresenterAction,
    HostTermination,
}
#[derive(Clone, Debug, PartialEq)]
pub struct McpFormOutcome {
    pub kind: McpFormOutcomeKind,
    pub source: McpFormOutcomeSource,
}
impl McpFormOutcome {
    pub fn human_admission(&self) -> bool {
        self.source == McpFormOutcomeSource::PresenterAction
    }
}

#[derive(Clone)]
pub struct McpFormInput {
    pub operation_id: String,
    pub input_id: String,
    pub round: u32,
    pub server: McpFormServer,
    pub message: String,
    pub schema: ValidatedMcpFormSchema,
    pub operation_signal: AbortSignal,
    /// Host reads a worker-owned atomic abort fence at the terminal latch, without an IPC round trip.
    pub operation_current: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub presentation_signal: AbortSignal,
    /// Remaining UI budget, measured by the host's monotonic clock.
    pub deadline_ms: f64,
}

pub trait McpFormPresentation: Send + Sync {
    fn signal(&self) -> AbortSignal;
    fn ready(&self) -> bool;
    fn present(&self, input: McpFormInput) -> BoxFuture<'_, McpFormOutcome>;
    /// Worker-backed presenters recheck host readiness before each network attempt.
    fn synchronize(&self) -> Option<BoxFuture<'_, ()>> {
        None
    }
}

pub type McpFormListener = Arc<dyn Fn() + Send + Sync>;
pub type McpFormUnsubscribe = Box<dyn FnOnce() + Send>;
pub struct McpFormBinding {
    pub epoch: String,
    pub signal: AbortSignal,
    pub capture: Arc<dyn Fn() -> Option<Arc<dyn McpFormPresentation>> + Send + Sync>,
    pub dispose: Arc<dyn Fn() + Send + Sync>,
    pub version: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub subscribe: Option<Arc<dyn Fn(McpFormListener) -> McpFormUnsubscribe + Send + Sync>>,
}

/// An enforced binding is part of the host-issued presenter identity, not a UI capability declaration.
pub fn bind_private_mcp_forms(ui: &Arc<dyn ExtensionUi>, binding: Arc<McpFormBinding>) {
    let signal = binding.signal.clone();
    crate::mrtr_presentations::register_mcp_mrtr_presentation(ui, signal, None, Some(binding));
}
pub fn private_mcp_forms(ui: &Arc<dyn ExtensionUi>) -> Option<Arc<McpFormBinding>> {
    crate::mrtr_presentations::get_mcp_mrtr_presentation(ui)?.private_forms
}

// DIVERGENCE(senpi core/mcp-form-types.ts bindMcpFormPresented): upstream keys the callback by the dialog
// options object, which Rust passes by value with no identity. The port keys it by the host UI the private
// dialog was shown on; MRTR consent shows at most one private form per binding at a time (consent queue).
static PRESENTED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, Arc<dyn Fn() + Send + Sync>>>,
> = std::sync::LazyLock::new(Default::default);
fn ui_key(ui: &Arc<dyn ExtensionUi>) -> usize {
    Arc::as_ptr(ui).cast::<()>() as usize
}
pub fn bind_mcp_form_presented(
    ui: &Arc<dyn ExtensionUi>,
    callback: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let mut presented = PRESENTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match callback {
        Some(callback) => presented.insert(ui_key(ui), callback),
        None => presented.remove(&ui_key(ui)),
    };
}
/// Called by a host once an ephemeral private dialog is actually on screen.
pub fn mark_mcp_form_presented(ui: &Arc<dyn ExtensionUi>) {
    let callback = PRESENTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&ui_key(ui));
    if let Some(callback) = callback {
        callback();
    }
}
