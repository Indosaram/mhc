//! Port of senpi mcp/elicitation.ts: legacy `elicitation/create` form mode.
//!
//! The client declares the capability as an EMPTY object (maximum server compatibility) and answers
//! mid tool-call by walking the flat-primitive schema through sequential ui primitives (input/select;
//! never custom UI). Without a UI the request fails with a processing error, never a user refusal. The
//! whole form is bounded by a timeout that resolves to cancel. URL mode is deliberately unsupported.
use crate::{
    form_types::{
        McpFormInput, McpFormOutcomeKind, McpFormServer, McpFormServerSource, private_mcp_forms,
    },
    modern::form_schema::validate_mrtr_form,
    request_origin::{LegacyMcpOperationLookup, legacy_mcp_operation},
    transport_sdk::McpClient,
};
use maho_ext_api::{ExtensionUi, ExtensionUiDialogOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Duration};

pub const MCP_ELICITATION_TIMEOUT_MS: u64 = 300000;
/// JSON-RPC `ErrorCode.InternalError`.
pub const INTERNAL_ERROR: i64 = -32603;
/// Declared empty on purpose: maximum server compatibility (form mode only).
pub fn mcp_client_elicitation_capability() -> Value {
    json!({"elicitation":{}})
}
pub fn mcp_client_capabilities(elicitation_enabled: bool) -> Value {
    if elicitation_enabled {
        mcp_client_elicitation_capability()
    } else {
        json!({})
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationResponse {
    pub action: ElicitationAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Map<String, Value>>,
}
/// A JSON-RPC error reply; never echoes the server's form body (`data` is always absent).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ElicitationRejection {
    pub code: i64,
    pub message: String,
}
impl ElicitationRejection {
    pub fn to_json(&self) -> Value {
        json!({"code": self.code, "message": self.message})
    }
}
fn reject() -> ElicitationRejection {
    ElicitationRejection {
        code: INTERNAL_ERROR,
        message: "Elicitation cannot be processed".into(),
    }
}
fn reject_private() -> ElicitationRejection {
    ElicitationRejection {
        code: INTERNAL_ERROR,
        message: "Elicitation processing unavailable".into(),
    }
}
fn response(action: ElicitationAction) -> ElicitationResponse {
    ElicitationResponse {
        action,
        content: None,
    }
}

/// The readiness half of upstream `McpElicitationUiProvider` (`enforced`, `eligible`, `subscribe`). A pooled
/// connection installs an aggregate over its leases (shared-connection.ts); otherwise readiness is read from
/// the connection's own UI.
pub type McpFormReadinessSubscribe = Arc<
    dyn Fn(Arc<dyn Fn() + Send + Sync>) -> crate::form_types::McpFormUnsubscribe + Send + Sync,
>;

#[derive(Clone)]
pub struct McpFormReadiness {
    pub enforced: Arc<dyn Fn() -> bool + Send + Sync>,
    pub eligible: Arc<dyn Fn() -> bool + Send + Sync>,
    pub subscribe: McpFormReadinessSubscribe,
}
/// Upstream `uiProvider?.enforced?.()`, falling back to the UI's own private binding.
pub fn mcp_forms_enforced(
    readiness: Option<&McpFormReadiness>,
    ui: Option<&Arc<dyn ExtensionUi>>,
) -> bool {
    match readiness {
        Some(readiness) => (readiness.enforced)(),
        None => ui.is_some_and(|ui| private_mcp_forms(ui).is_some()),
    }
}
/// Upstream `legacyMcpFormEligible(provider, enabled)`: a provider's `eligible` wins over the UI's binding.
pub fn legacy_mcp_form_eligible_with(
    readiness: Option<&McpFormReadiness>,
    ui: Option<&Arc<dyn ExtensionUi>>,
    enabled: bool,
) -> bool {
    if !enabled {
        return false;
    }
    match readiness {
        Some(readiness) => (readiness.eligible)(),
        None => legacy_mcp_form_eligible(ui, enabled),
    }
}

/// Whether legacy forms may be advertised: a host-enforced private binding must have a ready presenter.
pub fn legacy_mcp_form_eligible(ui: Option<&Arc<dyn ExtensionUi>>, enabled: bool) -> bool {
    if !enabled {
        return false;
    }
    match ui.and_then(private_mcp_forms) {
        None => true,
        Some(binding) => (binding.capture)().is_some_and(|presenter| (presenter.ready)()),
    }
}

async fn private_elicitation(
    client: &McpClient,
    params: &Value,
    request_id: &Value,
    timeout: Duration,
) -> Result<Value, ElicitationRejection> {
    let (origin, signal) = match legacy_mcp_operation(client) {
        LegacyMcpOperationLookup::One { origin, signal } => (origin, signal),
        LegacyMcpOperationLookup::None | LegacyMcpOperationLookup::Ambiguous => {
            return Err(reject_private());
        }
    };
    let Some(presenter) = origin
        .private_form
        .clone()
        .filter(|presenter| (presenter.ready)())
    else {
        return Err(reject_private());
    };
    let message = params.get("message").and_then(Value::as_str);
    let mode_ok = params
        .get("mode")
        .is_none_or(|mode| mode.as_str() == Some("form"));
    let schema = params
        .get("requestedSchema")
        .cloned()
        .unwrap_or(Value::Null);
    let (Some(message), true, true, Some(schema)) = (
        message,
        mode_ok,
        validate_mrtr_form(&schema),
        schema.as_object().cloned(),
    ) else {
        return Err(reject_private());
    };
    let id = request_id
        .as_str()
        .map_or_else(|| request_id.to_string(), str::to_owned);
    let operation = crate::wrap::McpHostSignal::follow(&signal);
    let outcome = (presenter.present)(McpFormInput {
        operation_id: id.clone(),
        input_id: id,
        round: 0,
        server: McpFormServer {
            label: origin.server_label.clone(),
            source: origin.server_source.unwrap_or(McpFormServerSource::User),
        },
        message: message.into(),
        schema: Value::Object(schema),
        operation_signal: operation.signal.clone(),
        operation_current: None,
        presentation_signal: presenter.signal.clone(),
        deadline_ms: timeout.as_secs_f64() * 1000.0,
    })
    .await;
    match outcome.kind {
        // The presenter answers with a validated `MrtrInputResponse` as JSON; re-validate before the server sees it.
        McpFormOutcomeKind::Result(result) if !signal.is_aborted() && origin.current() => {
            serde_json::from_value::<crate::modern::elicitation::MrtrInputResponse>(result)
                .and_then(serde_json::to_value)
                .map_err(|_| reject_private())
        }
        _ => Err(reject_private()),
    }
}

/// The client's `elicitation/create` handler (upstream `configureMcpElicitation`).
pub async fn handle_client_elicitation(
    client: &Arc<McpClient>,
    ui: Option<&Arc<dyn ExtensionUi>>,
    params: &Value,
    request_id: &Value,
    timeout: Duration,
) -> Result<Value, ElicitationRejection> {
    let enforced = client
        .protocol
        .private_forms
        .load(std::sync::atomic::Ordering::Acquire)
        || ui.is_some_and(|ui| private_mcp_forms(ui).is_some());
    // Unsolicited requests without an advertised capability never claim UI support.
    if client.protocol.legacy_form_eligible() == Some(false) {
        return Err(if enforced { reject_private() } else { reject() });
    }
    if enforced {
        // Private validation prevents error data from echoing malformed form bodies.
        return private_elicitation(client, params, request_id, timeout).await;
    }
    let result = handle_mcp_elicitation(ui.map(|ui| &**ui), params, timeout).await?;
    serde_json::to_value(result).map_err(|_| reject())
}

pub async fn handle_mcp_elicitation(
    ui: Option<&dyn ExtensionUi>,
    params: &Value,
    timeout: Duration,
) -> Result<ElicitationResponse, ElicitationRejection> {
    // URL mode is deliberately unsupported in v1 (form mode only).
    let Some(schema) = params.get("requestedSchema") else {
        return Err(reject());
    };
    let Some(ui) = ui else {
        return Err(reject());
    };
    run_elicitation_form(
        ui,
        params.get("message").and_then(Value::as_str).unwrap_or(""),
        schema,
        timeout,
    )
    .await
}
pub async fn run_elicitation_form(
    ui: &dyn ExtensionUi,
    message: &str,
    schema: &Value,
    timeout: Duration,
) -> Result<ElicitationResponse, ElicitationRejection> {
    match tokio::time::timeout(timeout, collect_form(ui, message, schema)).await {
        Ok(result) => result,
        Err(_) => Ok(response(ElicitationAction::Cancel)),
    }
}
async fn collect_form(
    ui: &dyn ExtensionUi,
    message: &str,
    schema: &Value,
) -> Result<ElicitationResponse, ElicitationRejection> {
    let mut content = Map::new();
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required = |name: &str| {
        schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| required.iter().any(|value| value.as_str() == Some(name)))
    };
    for (name, property) in properties {
        let title = format!(
            "{message} - {}",
            property
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(name)
        );
        let description = property.get("description").and_then(Value::as_str);
        let kind = property.get("type").and_then(Value::as_str);
        if kind == Some("boolean") {
            // confirm collapses cancellation into false, including automated RPC replies.
            // A selection preserves dismissal separately from a human's explicit No.
            let prompt = description.map_or_else(
                || title.clone(),
                |description| format!("{title}\n{description}"),
            );
            let Some(picked) = ui
                .select(
                    &prompt,
                    &["Yes".to_owned(), "No".to_owned()],
                    ExtensionUiDialogOptions::default(),
                )
                .await
            else {
                return Ok(response(ElicitationAction::Cancel));
            };
            // The thread gateway also accepts y/n and true/false, ignoring case and whitespace.
            let flag = match maho_ai::utils::js::trim(&picked).to_lowercase().as_str() {
                "yes" | "y" | "true" => true,
                "no" | "n" | "false" => false,
                _ => return Err(reject()),
            };
            content.insert(name.clone(), Value::Bool(flag));
            continue;
        }
        if let Some(options) = property
            .get("enum")
            .and_then(Value::as_array)
            .filter(|options| !options.is_empty())
        {
            let options = options
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
                .collect::<Vec<_>>();
            let Some(picked) = ui
                .select(&title, &options, ExtensionUiDialogOptions::default())
                .await
            else {
                return Ok(response(ElicitationAction::Cancel));
            };
            content.insert(name.clone(), json!(picked));
            continue;
        }
        let Some(answer) = ui
            .input(&title, description, ExtensionUiDialogOptions::default())
            .await
        else {
            return Ok(response(ElicitationAction::Cancel));
        };
        if answer.is_empty() {
            if required(name) {
                return Err(reject());
            }
            continue;
        }
        let value = if matches!(kind, Some("number" | "integer")) {
            let number = maho_ai::utils::js::string_to_number(&answer);
            if !number.is_finite() {
                return Err(reject());
            }
            maho_ai::utils::js::json_number(number)
        } else {
            json!(answer)
        };
        content.insert(name.clone(), value);
    }
    // Even a fieldless form needs an explicit submission rather than an automatic accept.
    if content.is_empty()
        && ui
            .select(
                message,
                &["Submit".to_owned()],
                ExtensionUiDialogOptions::default(),
            )
            .await
            .as_deref()
            != Some("Submit")
    {
        return Ok(response(ElicitationAction::Cancel));
    }
    Ok(ElicitationResponse {
        action: ElicitationAction::Accept,
        content: Some(content),
    })
}
