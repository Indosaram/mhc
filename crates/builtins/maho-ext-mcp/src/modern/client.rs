//! Port of senpi mcp/modern/client.ts: configure a fresh client's form policy before its first request.
use crate::{
    elicitation::{McpFormReadiness, legacy_mcp_form_eligible_with, mcp_forms_enforced},
    transport_sdk::McpClient,
};
use maho_ext_api::ExtensionUi;
use std::sync::Arc;

/// Records whether private forms are enforced and whether legacy forms may be advertised. The `initialize`
/// capabilities and the `elicitation/create` handler read this state (upstream `mcpClientCapabilities` and
/// `configureMcpElicitation`).
pub fn create_mcp_request_client(
    client: &McpClient,
    ui: Option<&Arc<dyn ExtensionUi>>,
    readiness: Option<&McpFormReadiness>,
    elicitation_enabled: bool,
) {
    client.configure_requests(
        mcp_forms_enforced(readiness, ui),
        legacy_mcp_form_eligible_with(readiness, ui, elicitation_enabled),
    );
}
