//! Port of senpi packages/coding-agent/src/core/execution/preflight-carry.ts.

use std::sync::{Arc, LazyLock, Mutex};

use maho_agent::execution::tool_host::AfterToolFreeze;
use serde_json::Value;

pub type PreflightTransferFn = Arc<dyn Fn(&Value, &mut Value) + Send + Sync>;

static HOOKS: LazyLock<Mutex<Vec<PreflightTransferFn>>> = LazyLock::new(Default::default);

/// Register a transfer hook (for permission-system dispatch approval, terminal monitor parent, etc.).
pub fn register_preflight_carry_hook(hook: PreflightTransferFn) {
    HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(hook);
}

/// Transfers any preflight facts bound to `prepared` arguments onto `frozen` arguments (`senpi#5795`).
pub fn carry_preflight_facts(prepared: &Value, frozen: &mut Value) {
    let hooks = HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    for hook in hooks {
        hook(prepared, frozen);
    }
}

pub fn preflight_carry_fn() -> AfterToolFreeze {
    Arc::new(carry_preflight_facts)
}
