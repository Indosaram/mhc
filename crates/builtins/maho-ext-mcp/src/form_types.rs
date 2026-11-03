//! Port of senpi core/mcp-form-types.ts: the host-issued private MCP form presenter contract.
//!
//! The contract lives in maho-core (`maho_core::mcp_form_types`), where the RPC and interactive hosts bind
//! presenters and the runner inherits them; this module re-exports it. The MCP form schema
//! (`ValidatedMcpFormSchema`) and the MRTR input response (`MrtrInputResponse`) stay in this crate and cross
//! the boundary as validated JSON.
pub use maho_core::mcp_form_types::{
    McpFormBinding, McpFormEnvelope, McpFormInput, McpFormOutcome, McpFormOutcomeKind,
    McpFormOutcomeSource, McpFormPresentation, McpFormPurpose, McpFormRequest, McpFormResolved,
    McpFormResolvedReason, McpFormScope, McpFormServer, McpFormUnsubscribe, McpPresenterKind,
    McpServerSource as McpFormServerSource, bind_mcp_form_presented, bind_private_mcp_forms,
    mark_mcp_form_presented, private_mcp_forms,
};
