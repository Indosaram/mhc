//! Port of senpi mcp/modern/client.ts: configure a fresh client's form policy before its first request.
use crate::{
    elicitation::legacy_mcp_form_eligible, form_types::private_mcp_forms, transport_sdk::McpClient,
};
use maho_ext_api::ExtensionUi;
use std::sync::Arc;

/// Records whether private forms are enforced and whether legacy forms may be advertised. The `initialize`
/// capabilities and the `elicitation/create` handler read this state (upstream `mcpClientCapabilities` and
/// `configureMcpElicitation`).
pub fn create_mcp_request_client(
    client: &McpClient,
    ui: Option<&Arc<dyn ExtensionUi>>,
    elicitation_enabled: bool,
) {
    let private_forms = ui.is_some_and(|ui| private_mcp_forms(ui).is_some());
    client.configure_requests(
        private_forms,
        legacy_mcp_form_eligible(ui, elicitation_enabled),
    );
}
