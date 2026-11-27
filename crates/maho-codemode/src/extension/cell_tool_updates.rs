use crate::tool::nested_tool_output::CellToolUpdateSink;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The live-update sinks of the subprocess-kernel cells running right now, keyed by the same
/// per-run secret as `CellKernelTools`: a bridge call carrying it streams its tool's partial
/// results into that cell (`#6167`).
#[derive(Default)]
pub struct CellToolUpdates {
    by_token: Mutex<HashMap<String, Arc<dyn CellToolUpdateSink>>>,
}

impl CellToolUpdates {
    pub fn bind(
        this: &Arc<Self>,
        token: impl Into<String>,
        sink: Arc<dyn CellToolUpdateSink>,
    ) -> Box<dyn FnOnce() + Send + Sync> {
        let token = token.into();
        this.by_token
            .lock()
            .expect("cell tool updates lock")
            .insert(token.clone(), Arc::clone(&sink));
        let weak = Arc::downgrade(this);
        Box::new(move || {
            if let Some(map) = weak.upgrade() {
                let mut guard = map.by_token.lock().expect("cell tool updates lock");
                if guard
                    .get(&token)
                    .is_some_and(|current| Arc::ptr_eq(current, &sink))
                {
                    guard.remove(&token);
                }
            }
        })
    }

    pub fn sink_for(&self, token: Option<&str>) -> Option<Arc<dyn CellToolUpdateSink>> {
        let token = token?;
        self.by_token
            .lock()
            .expect("cell tool updates lock")
            .get(token)
            .cloned()
    }
}
