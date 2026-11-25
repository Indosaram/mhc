use super::{
    eval_tool_options::EvalKernelManager, nested_tool_output::CellToolUpdateSink,
    types::EvalLanguage,
};
use std::sync::Arc;

pub struct BridgeCellBinding {
    pub token: Option<String>,
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl BridgeCellBinding {
    /// First thing a settling cell runs: from then on a call carrying the token gets no kernel
    /// tools and streams nowhere.
    pub fn release(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// Subprocess kernels call the host over the bridge, outside this cell's async context. Their
/// calls carry a fresh secret minted for this run and sent only to this cell's kernel, never the
/// model-visible cell id. JS calls already run inside this cell's scope (`#1754`).
pub fn bind_bridge_cell(
    manager: &dyn EvalKernelManager,
    language: EvalLanguage,
    tool_updates: Arc<dyn CellToolUpdateSink>,
) -> BridgeCellBinding {
    if language == EvalLanguage::Js {
        return BridgeCellBinding {
            token: None,
            release: None,
        };
    }
    let token = uuid::Uuid::new_v4().to_string();
    let release_updates = manager.bind_cell_tool_updates(&token, tool_updates);
    BridgeCellBinding {
        token: Some(token),
        release: release_updates,
    }
}
