//! Port of senpi core/mcp-mrtr-presentations.ts: host-issued MCP form presentations keyed by UI identity.
//!
//! senpi keys a `WeakMap` by the host UI object. The port keys by the `Arc<dyn ExtensionUi>` allocation and
//! holds only `Weak`s, so a dropped UI never resolves, never stays alive through the registry, and a reused
//! address cannot inherit a stale entry.
use crate::form_types::McpFormBinding;
use maho_ext_api::{AbortSignal, ExtensionUi};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

/// Local host references only. RPC capabilities and serialized UI objects cannot enter this registry.
#[derive(Clone)]
pub struct McpMrtrPresentation {
    pub ui: Arc<dyn ExtensionUi>,
    pub signal: AbortSignal,
    pub admit: Option<Arc<dyn Fn() + Send + Sync>>,
    pub presented: Option<Arc<dyn Fn() + Send + Sync>>,
    pub private_forms: Option<Arc<McpFormBinding>>,
}

struct Entry {
    /// The UI this entry is registered under (the key's liveness).
    owner: Weak<dyn ExtensionUi>,
    /// The raw host UI private dialogs use; differs from `owner` for an inherited wrapper.
    ui: Weak<dyn ExtensionUi>,
    signal: AbortSignal,
    admit: Option<Arc<dyn Fn() + Send + Sync>>,
    presented: Option<Arc<dyn Fn() + Send + Sync>>,
    private_forms: Option<Arc<McpFormBinding>>,
}
static PRESENTATIONS: std::sync::LazyLock<Mutex<HashMap<usize, Entry>>> =
    std::sync::LazyLock::new(Default::default);

fn key(ui: &Arc<dyn ExtensionUi>) -> usize {
    Arc::as_ptr(ui).cast::<()>() as usize
}
fn store(owner: &Arc<dyn ExtensionUi>, presentation: &McpMrtrPresentation) {
    let mut presentations = PRESENTATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    presentations.retain(|_, entry| entry.owner.strong_count() > 0);
    presentations.insert(
        key(owner),
        Entry {
            owner: Arc::downgrade(owner),
            ui: Arc::downgrade(&presentation.ui),
            signal: presentation.signal.clone(),
            admit: presentation.admit.clone(),
            presented: presentation.presented.clone(),
            private_forms: presentation.private_forms.clone(),
        },
    );
}

pub fn register_mcp_mrtr_presentation(
    ui: &Arc<dyn ExtensionUi>,
    signal: AbortSignal,
    admit: Option<Arc<dyn Fn() + Send + Sync>>,
    private_forms: Option<Arc<McpFormBinding>>,
) -> Arc<dyn ExtensionUi> {
    store(
        ui,
        &McpMrtrPresentation {
            ui: ui.clone(),
            signal,
            admit,
            presented: None,
            private_forms,
        },
    );
    ui.clone()
}

pub fn get_mcp_mrtr_presentation(ui: &Arc<dyn ExtensionUi>) -> Option<McpMrtrPresentation> {
    let presentations = PRESENTATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = presentations.get(&key(ui))?;
    let owner = entry.owner.upgrade()?;
    if key(&owner) != key(ui) || entry.signal.is_aborted() {
        return None;
    }
    Some(McpMrtrPresentation {
        ui: entry.ui.upgrade()?,
        signal: entry.signal.clone(),
        admit: entry.admit.clone(),
        presented: entry.presented.clone(),
        private_forms: entry.private_forms.clone(),
    })
}

/// Preserve host identity across runner wrapping; private dialogs still use the raw host UI.
pub fn inherit_mcp_mrtr_presentation(
    source: &Arc<dyn ExtensionUi>,
    wrapper: Arc<dyn ExtensionUi>,
) -> Arc<dyn ExtensionUi> {
    if let Some(presentation) = get_mcp_mrtr_presentation(source) {
        store(&wrapper, &presentation);
    }
    wrapper
}
