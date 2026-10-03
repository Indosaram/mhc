pub use maho_ext_api::{KernelToolInvokeRequest as KernelToolsInvokeRequest, KernelToolInvokeOptions as KernelToolsInvokeOptions, KernelToolInvokeScope as KernelToolsInvokeScope};
pub use super::kernel_tools_errors::{KernelToolErrorCode, KernelToolHostDenial, KernelToolHostDenialReason};
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub language: String,
    pub kernel_generation: u64,
    pub definition_revision: u64,
}

pub const KERNEL_TOOLS_INVOKE_SCOPE: bool = true;
pub const KERNEL_TOOLS_UNSUPPORTED_MESSAGE: &str = "Kernel tools require a live JavaScript worker context";
