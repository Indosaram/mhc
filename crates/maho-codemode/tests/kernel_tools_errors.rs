use maho_codemode::kernels::{js::kernel_tools_errors::*, kernel_tools_unavailable::reject_kernel_tools_unavailable};
use serde_json::json;

#[test]
fn error_codes_preserve_wire_order() {
    assert_eq!(serde_json::to_value(KERNEL_TOOL_ERROR_CODES).unwrap(), json!(["tools_unavailable", "invalid_tool_definition", "reserved_tool_name", "tool_name_collision", "kernel_tool_stale", "kernel_tool_missing", "kernel_tool_failed", "kernel_tool_recursion", "kernel_tool_host_denied"]));
}

#[test]
fn errors_without_details_omit_details() {
    let error = kernel_tool_error(KernelToolErrorCode::KernelToolMissing, "missing", None);
    assert_eq!(serde_json::to_value(&error).unwrap(), json!({"name":"KernelToolError", "code":"kernel_tool_missing", "message":"missing"}));
    assert_eq!(error.to_string(), "missing");
}

#[test]
fn host_denial_preserves_structured_payload() {
    let details = KernelToolHostDenial { tool: "read".into(), call_id: "call-1".into(), reason: KernelToolHostDenialReason::Deny };
    let error = kernel_tool_error(KernelToolErrorCode::KernelToolHostDenied, "denied", Some(details));
    assert_eq!(serde_json::to_value(&error).unwrap()["details"], json!({"tool":"read", "call_id":"call-1", "reason":"deny"}));
}

#[tokio::test]
async fn subprocess_kernel_tools_reject_as_unavailable() {
    let error = reject_kernel_tools_unavailable().await.unwrap_err();
    assert_eq!(error.code, KernelToolErrorCode::ToolsUnavailable);
    assert_eq!(error.name, "KernelToolError");
}
