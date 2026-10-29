//! Port of `omo-senpi/src/extension/tool-capture-registry.ts` at pin `77f3067f1`.
//!
//! Upstream wraps `pi.registerTool` because `pi.getAllTools()` returns `ToolInfo` without the
//! execute closure. Native `LoadedExtension.tools` already carries each full `ToolDefinition`
//! (including `execute`) after registration, so capture is a snapshot of that list instead of a
//! wrapper (ledger N/A reason).

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::ToolDefinition;

#[derive(Clone, Default)]
pub struct ToolCaptureRegistry {
    tools: Arc<Mutex<Vec<ToolDefinition>>>,
}

impl ToolCaptureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn capture(&self, tools: Vec<ToolDefinition>) {
        *self.tools.lock().unwrap_or_else(PoisonError::into_inner) = tools;
    }

    /// Newest-last, matching upstream's registration order.
    pub fn get_captured_tools(&self) -> Vec<ToolDefinition> {
        self.tools.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}
