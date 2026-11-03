//! Port of senpi packages/coding-agent/src/core/mcp-form-types.ts.
//!
//! MCP private terminal form consent: the trusted presenter scope, the request/resolved wire records, the
//! outcome a presenter reports, and the epoch-bound binding a host registers on its UI presentation.
//!
//! PERF-DIVERGENCE(senpi WeakMap): `presentedCallbacks` is keyed by the options object's identity upstream.
//! Here the options are an `Arc`; the callback is keyed by its data pointer and holds a `Weak`, so a dropped
//! options value never fires a stale callback.
//!
//! The MCP form schema (`ValidatedMcpFormSchema`) and the MRTR input response (`MrtrInputResponse`) belong to
//! the MCP extension crate, which core does not depend on; they cross this boundary as validated JSON.

use crate::mcp_mrtr_presentations::{get_mcp_mrtr_presentation, register_mcp_mrtr_presentation};
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::types::ExtensionUi;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::any::Any;
use std::sync::{Arc, Mutex, OnceLock, Weak};

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

/// The host-issued identity a private form is presented under (senpi#355f87f).
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
#[serde(rename_all = "snake_case")]
pub enum McpServerSource {
    User,
    Project,
    Plugin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpFormServer {
    pub label: String,
    pub source: McpServerSource,
}

/// `type: "mcp_form_request"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", rename = "mcp_form_request")]
pub struct McpFormRequest {
    #[serde(flatten)]
    pub envelope: McpFormEnvelope,
    pub server: McpFormServer,
    pub message: String,
    /// `ValidatedMcpFormSchema`.
    pub schema: Value,
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

/// `type: "mcp_form_resolved"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", rename = "mcp_form_resolved")]
pub struct McpFormResolved {
    pub session_id: String,
    pub grant_id: String,
    pub dialog_id: String,
    pub reason: McpFormResolvedReason,
}

#[derive(Clone, Debug, PartialEq)]
pub enum McpFormOutcomeKind {
    /// `MrtrInputResponse`.
    Result(Value),
    ProcessingFailure,
    OperationAborted,
}

/// Who ended the form: a presenter action is human admission, a host termination never is.
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
    /// `humanAdmission` is fixed by the source, so it cannot disagree with it.
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
    /// `ValidatedMcpFormSchema`.
    pub schema: Value,
    pub operation_signal: AbortSignal,
    /// Host reads a worker-owned atomic abort fence at the terminal latch, without an IPC round trip.
    pub operation_current: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub presentation_signal: AbortSignal,
    /// Remaining UI budget, measured by the host's monotonic clock.
    pub deadline_ms: f64,
}

#[derive(Clone)]
pub struct McpFormPresentation {
    pub signal: AbortSignal,
    pub ready: Arc<dyn Fn() -> bool + Send + Sync>,
    pub present: Arc<dyn Fn(McpFormInput) -> BoxFuture<'static, McpFormOutcome> + Send + Sync>,
    /// Worker-backed presenters recheck host readiness before each network attempt.
    pub synchronize: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
}

pub type McpFormUnsubscribe = Box<dyn FnOnce() + Send>;
pub type McpFormSubscribe =
    Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> McpFormUnsubscribe + Send + Sync>;

#[derive(Clone)]
pub struct McpFormBinding {
    pub epoch: String,
    pub signal: AbortSignal,
    pub capture: Arc<dyn Fn() -> Option<McpFormPresentation> + Send + Sync>,
    pub dispose: Arc<dyn Fn() + Send + Sync>,
    pub version: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub subscribe: Option<McpFormSubscribe>,
}

type PresentedCallback = Box<dyn FnOnce() + Send>;
struct Presented {
    options: Weak<dyn Any + Send + Sync>,
    callback: PresentedCallback,
}
static PRESENTED_CALLBACKS: OnceLock<Mutex<Vec<Presented>>> = OnceLock::new();

fn presented_callbacks() -> std::sync::MutexGuard<'static, Vec<Presented>> {
    PRESENTED_CALLBACKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn options_key(options: &Arc<dyn Any + Send + Sync>) -> *const () {
    Arc::as_ptr(options).cast::<()>()
}

fn take_presented(options: &Arc<dyn Any + Send + Sync>) -> Option<PresentedCallback> {
    let mut callbacks = presented_callbacks();
    callbacks.retain(|entry| entry.options.strong_count() > 0);
    let index = callbacks
        .iter()
        .position(|entry| entry.options.as_ptr().cast::<()>() == options_key(options))?;
    Some(callbacks.swap_remove(index).callback)
}

pub fn bind_mcp_form_presented(
    options: &Arc<dyn Any + Send + Sync>,
    callback: Option<PresentedCallback>,
) {
    drop(take_presented(options));
    if let Some(callback) = callback {
        presented_callbacks().push(Presented {
            options: Arc::downgrade(options),
            callback,
        });
    }
}

/// Fires the bound callback once; later calls for the same options are no-ops.
pub fn mark_mcp_form_presented(options: Option<&Arc<dyn Any + Send + Sync>>) {
    let Some(options) = options else {
        return;
    };
    // Taken under the lock, run outside it, so a callback may bind again.
    if let Some(callback) = take_presented(options) {
        callback();
    }
}

/// An enforced binding is part of the host-issued presenter identity, not a UI capability declaration.
pub fn bind_private_mcp_forms(ui: Arc<dyn ExtensionUi>, binding: McpFormBinding) {
    let signal = binding.signal.clone();
    register_mcp_mrtr_presentation(ui, signal, None, Some(binding));
}

pub fn private_mcp_forms(ui: &Arc<dyn ExtensionUi>) -> Option<McpFormBinding> {
    get_mcp_mrtr_presentation(ui)?.private_forms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_mrtr_presentations::{inherit_mcp_mrtr_presentation, tests::Ui};
    use maho_ai::utils::abort::AbortController;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn binding(signal: AbortSignal) -> McpFormBinding {
        McpFormBinding {
            epoch: "e1".into(),
            signal,
            capture: Arc::new(|| None),
            dispose: Arc::new(|| {}),
            version: None,
            subscribe: None,
        }
    }

    #[test]
    fn private_forms_follow_the_host_ui_through_wrapping_until_aborted() {
        let host: Arc<dyn ExtensionUi> = Arc::new(Ui);
        let abort = AbortController::new();
        bind_private_mcp_forms(host.clone(), binding(abort.signal()));
        assert_eq!(private_mcp_forms(&host).expect("bound").epoch, "e1");
        let wrapper = inherit_mcp_mrtr_presentation(&host, Arc::new(Ui));
        assert_eq!(private_mcp_forms(&wrapper).expect("inherited").epoch, "e1");
        assert!(private_mcp_forms(&(Arc::new(Ui) as Arc<dyn ExtensionUi>)).is_none());
        abort.abort(None);
        assert!(private_mcp_forms(&host).is_none());
    }

    #[test]
    fn the_presented_callback_fires_once_per_binding() {
        let options: Arc<dyn Any + Send + Sync> = Arc::new(());
        let fired = Arc::new(AtomicUsize::new(0));
        let count = fired.clone();
        bind_mcp_form_presented(
            &options,
            Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            })),
        );
        mark_mcp_form_presented(Some(&options));
        mark_mcp_form_presented(Some(&options));
        mark_mcp_form_presented(None);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        bind_mcp_form_presented(&options, Some(Box::new(|| panic!("unbound"))));
        bind_mcp_form_presented(&options, None);
        mark_mcp_form_presented(Some(&options));
    }

    #[test]
    fn wire_records_keep_their_type_tags_and_camel_case_fields() {
        let resolved = McpFormResolved {
            session_id: "s".into(),
            grant_id: "g".into(),
            dialog_id: "d".into(),
            reason: McpFormResolvedReason::OperationAborted,
        };
        assert_eq!(
            serde_json::to_value(&resolved).expect("json"),
            json!({"type":"mcp_form_resolved","sessionId":"s","grantId":"g","dialogId":"d","reason":"operation_aborted"})
        );
        let request: McpFormRequest = serde_json::from_value(json!({
            "type":"mcp_form_request","sessionId":"s","bindingEpoch":"b","attachmentEpoch":"a","grantId":"g",
            "dialogId":"d","dialogToken":"t","formRevision":"r","server":{"label":"srv","source":"plugin"},
            "message":"m","schema":{"type":"object"},"deadlineAtMs":5.0
        }))
        .expect("request");
        assert_eq!(request.server.source, McpServerSource::Plugin);
        assert_eq!(request.envelope.dialog_token, "t");
        let scope: McpFormScope = serde_json::from_value(json!({
            "hostInstance":"h","durableSessionId":"s","bindingEpoch":"b","attachmentId":"a","attachmentEpoch":"ae",
            "connectionId":"c","connectionEpoch":"ce","presenterPrincipalId":"p","presenterGeneration":"pg",
            "presentationBindingId":"pb","presenterKind":"local_tui","purpose":"mcp_form"
        }))
        .expect("scope");
        assert_eq!(scope.presenter_kind, McpPresenterKind::LocalTui);
    }

    #[test]
    fn human_admission_is_fixed_by_the_outcome_source() {
        let outcome = McpFormOutcome {
            kind: McpFormOutcomeKind::OperationAborted,
            source: McpFormOutcomeSource::HostTermination,
        };
        assert!(!outcome.human_admission());
    }
}
