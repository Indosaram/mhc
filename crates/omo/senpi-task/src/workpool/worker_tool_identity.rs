use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct WorkpoolYieldToolIdentity(Arc<Mutex<HashSet<String>>>);

impl WorkpoolYieldToolIdentity {
    pub fn mark(&self, identity: impl Into<String>) -> String {
        let identity = identity.into();
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(identity.clone());
        identity
    }

    pub fn is_marked(&self, identity: &str) -> bool {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(identity)
    }
}

pub fn mark_workpool_yield_tool(identity: impl Into<String>, registry: &WorkpoolYieldToolIdentity) -> String { registry.mark(identity) }
pub fn is_workpool_yield_tool(identity: &str, registry: &WorkpoolYieldToolIdentity) -> bool { registry.is_marked(identity) }

#[cfg(test)] mod tests { use super::*; #[test] fn identity_is_runtime_only() { let registry = WorkpoolYieldToolIdentity::default(); registry.mark("yield"); assert!(registry.is_marked("yield")); assert!(!registry.is_marked("other")); } }
