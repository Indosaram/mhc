use super::js::kernel_tools_errors::{KernelToolError, KernelToolErrorCode, kernel_tool_error};

pub async fn reject_kernel_tools_unavailable() -> Result<std::convert::Infallible, KernelToolError> {
    Err(kernel_tool_error(KernelToolErrorCode::ToolsUnavailable, "Kernel tools are JavaScript-only", None))
}
